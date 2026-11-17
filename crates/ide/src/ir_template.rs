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

pub(crate) type BodyAnalysis = hir::template_instance::TemplateInstance;
mod body_memo;
mod render_edit;
pub(crate) use render_edit::boundary::prewarm as prewarm_template_boundary;

#[cfg(test)]
thread_local! {
    static REUSE_RENDERING: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

#[cfg(test)]
pub(crate) fn with_full_template_rendering<T>(run: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            REUSE_RENDERING.set(self.0);
        }
    }
    let _reset = Reset(REUSE_RENDERING.replace(false));
    run()
}

fn has_structural_reads(template: &hir::Template, snapshot: &AnalysisSnapshot) -> bool {
    use rules::template::{TemplateFragment, TemplateInstruction, TemplateOperand};
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
            if schema.zip(key.as_ref()).is_some_and(|(schema,key)|hir::checking::field_candidates(ir,schema,key,rules::ir::Shape::Scalar,&WorkspaceFacts{snapshot}).is_empty() && !hir::checking::field_candidates(ir,schema,key,rules::ir::Shape::Block,&WorkspaceFacts{snapshot}).is_empty()) {return true;}
            schema.zip(key).is_some_and(|(schema,key)|hir::checking::field_candidates(ir,schema,&key,rules::ir::Shape::Scalar,&WorkspaceFacts{snapshot})
                .iter().any(|id| {
                    let field = ir.field(*id);
                    hir::template::template_kind(ir,field.key).is_some() || ir_semantic::has_field_query(ir,field.key)
                        || matches!(field.value,rules::ir::FieldValue::Scalar(matcher) if ir_semantic::has_field_query(ir, matcher))
                }))
        }
    }))
}

pub(crate) fn needs_body_analysis(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
) -> bool {
    let Some(template) = summary.template.as_ref() else {
        return false;
    };
    let facts = WorkspaceFacts { snapshot };
    let memo = rules::ir::SymbolFacts::template_memo(&facts);
    let key = format!("body-policy:{:?}:{:?}", template.kind, template.name);
    if let Some(memo) = &memo
        && let Some(value) = memo.get::<bool>(&key)
    {
        return *value;
    }
    let value = has_structural_reads(template, snapshot);
    if let Some(memo) = memo {
        memo.insert(key, std::sync::Arc::new(value), 1);
    }
    value
}

/// Shared structural specialization for candidate filtering, diagnostics and explanations.
pub(crate) fn analyse_body(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    override_value: Option<(&str, &str)>,
    cancellation: &CancellationToken,
) -> Result<Option<std::sync::Arc<BodyAnalysis>>, Cancelled> {
    cancellation.checkpoint()?;
    let overrides = override_value
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();
    analyse_body_with_bindings(snapshot, source, invocation, &overrides, &[], cancellation)
}

fn analyse_body_with_bindings(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    overrides: &BTreeMap<String, String>,
    markers: &[String],
    cancellation: &CancellationToken,
) -> Result<Option<std::sync::Arc<BodyAnalysis>>, Cancelled> {
    analyse_body_with_argument_edit(
        snapshot,
        source,
        invocation,
        overrides,
        markers,
        None,
        cancellation,
        None,
    )
    .map(|body| {
        body.map(|body| match body {
            render_edit::RenderValidation::Body(body) => body,
            render_edit::RenderValidation::Rejected(_) => {
                unreachable!("ordinary validation retains its body")
            }
        })
    })
}

struct ArgumentEdit {
    key_range: text::TextRange,
    value: String,
    raw: String,
}

#[allow(clippy::too_many_arguments)]
fn analyse_body_with_argument_edit(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    overrides: &BTreeMap<String, String>,
    markers: &[String],
    edit: Option<&ArgumentEdit>,
    cancellation: &CancellationToken,
    reject_parameter: Option<&str>,
) -> Result<Option<render_edit::RenderValidation>, Cancelled> {
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
    let Some(template) = rules::ir::SymbolFacts::template(&facts, type_id, &invocation.key) else {
        return Ok(None);
    };
    let Some(schema) = hir::template::template_body(ir, type_id) else {
        return Ok(None);
    };
    let mut bindings = BTreeMap::new();
    let mut raw = BTreeMap::new();
    let mut present = BTreeSet::new();
    for argument in source
        .properties_in_range(invocation.range)
        .filter(|argument| {
            argument.path.len() == invocation.path.len() + 1
                && argument.path.starts_with(&invocation.path)
                && invocation.range.start() <= argument.range.start()
                && argument.range.end() <= invocation.range.end()
        })
    {
        let name = argument.key.to_ascii_lowercase();
        present.insert(name.clone());
        if let Some(edit) = edit.filter(|edit| edit.key_range == argument.key_range) {
            bindings.insert(name.clone(), edit.value.clone());
            raw.insert(name, edit.raw.clone());
        } else if let Some(scalar) = &argument.scalar {
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
    let state = invocation_state(source, invocation);
    // Instances keep root-relative byte maps. Caller locations are supplied at
    // projection time, so identical bindings/scope may share an instance.
    let memo = markers
        .is_empty()
        .then(|| rules::ir::SymbolFacts::template_memo(&facts))
        .flatten();
    let key = memo.as_ref().map(|_| {
        format!(
            "instance:{kind}:{}:{schema:?}:{state:?}:{bindings:?}:{raw:?}:{present:?}",
            invocation.key
        )
    });
    if let (Some(memo), Some(key)) = (&memo, &key)
        && let Some(body) = memo.get::<BodyAnalysis>(key)
    {
        return Ok(Some(render_edit::RenderValidation::Body(body)));
    }
    let shared_work = if let (Some(memo), Some(key)) = (&memo, &key) {
        match body_memo::acquire(memo, key, cancellation)? {
            body_memo::Claim::Ready(body) => {
                return Ok(Some(render_edit::RenderValidation::Body(body)));
            }
            body_memo::Claim::Build(builder) => Some(builder),
        }
    } else {
        None
    };
    let mut checkpoint = || cancellation.checkpoint();
    let body = if let Some(parameter) = reject_parameter {
        let Some(body) = hir::template_instance::instantiate_until_syntax_rejection(
            ir,
            &facts,
            template,
            &bindings,
            &raw,
            &present,
            schema,
            state,
            markers,
            parameter,
            &mut checkpoint,
        )?
        else {
            return Ok(Some(render_edit::RenderValidation::Rejected(
                Default::default(),
            )));
        };
        std::sync::Arc::new(body)
    } else {
        std::sync::Arc::new(hir::template_instance::instantiate(
            ir,
            &facts,
            template,
            &bindings,
            &raw,
            &present,
            schema,
            state,
            markers,
            hir::template_instance::InstanceGoal::Validation,
            &mut || cancellation.checkpoint(),
        )?)
    };
    cancellation.checkpoint()?;
    if let (Some(memo), Some(key)) = (memo, key)
        && body.coverage.is_complete()
    {
        let bytes = body
            .rendered
            .text
            .len()
            .saturating_mul(4)
            .saturating_add(body.rendered.pieces.len().saturating_mul(128))
            .saturating_add(
                body.rendered
                    .pieces
                    .iter()
                    .filter_map(|p| p.binding_source.as_ref())
                    .map(|map| map.offsets.len().saturating_mul(4))
                    .sum::<usize>(),
            )
            .saturating_add(body.hir.syntax().tree().node_count().saturating_mul(256))
            .saturating_add(
                body.rendered
                    .calls
                    .iter()
                    .map(|call| call.kind.len() + call.name.len() + 128)
                    .sum::<usize>(),
            );
        let mut views = BTreeSet::new();
        let trace_bytes = body
            .rendered
            .calls
            .iter()
            .filter(|call| views.insert(std::sync::Arc::as_ptr(&call.site.view) as usize))
            .map(|call| {
                call.site
                    .view
                    .text
                    .len()
                    .saturating_mul(4)
                    .saturating_add(call.site.view.pieces.len().saturating_mul(128))
            })
            .sum::<usize>();
        memo.insert(key, body.clone(), bytes.saturating_add(trace_bytes));
    }
    if let Some(builder) = shared_work {
        builder.publish(body.clone());
    }
    Ok(Some(render_edit::RenderValidation::Body(body)))
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

/// Exact inverse projection for semantic facts and edits. Display fallbacks are
/// deliberately excluded: a generated identifier may span several bindings.
pub(crate) fn project_source_range(
    body: &BodyAnalysis,
    range: text::TextRange,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
) -> Option<text::TextRange> {
    hir::template_instance::project_source_range(
        &body.rendered,
        range,
        source.syntax(),
        source
            .properties_in_range(invocation.range)
            .filter(|argument| {
                argument.path.len() == invocation.path.len() + 1
                    && argument.path.starts_with(&invocation.path)
                    && invocation.range.start() <= argument.range.start()
                    && argument.range.end() <= invocation.range.end()
            }),
    )
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
        analyse_body_with_bindings(snapshot, source, invocation, witness, &[], cancellation)?
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
        coverage: body.coverage.clone(),
    })
}

pub(crate) fn candidate_interpretations(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    summary: &engine::DynamicDefinitionSummary,
    witness: &hir::template_relations::Witness,
    parameter: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<crate::types::TemplateInterpretation>, Cancelled> {
    if !needs_body_analysis(snapshot, summary) {
        return Ok(Vec::new());
    }
    Ok(
        analyse_body_with_bindings(snapshot, source, invocation, witness, &[], cancellation)?
            .map(|body| body_interpretations(&body, parameter))
            .unwrap_or_default(),
    )
}

/// Original query declarations used by a concrete Template candidate.
pub(crate) fn candidate_query_fields(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    summary: &engine::DynamicDefinitionSummary,
    witness: &hir::template_relations::Witness,
    parameter: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<FieldId>, Cancelled> {
    if !needs_body_analysis(snapshot, summary) {
        return Ok(Vec::new());
    }
    let Some(body) =
        analyse_body_with_bindings(snapshot, source, invocation, witness, &[], cancellation)?
    else {
        return Ok(Vec::new());
    };
    let ir = snapshot.ir();
    let parameter = parameter.to_ascii_lowercase();
    let mut sources = BTreeSet::new();
    for property in body.hir.properties() {
        cancellation.checkpoint()?;
        let key = body
            .rendered
            .dependencies(property.key_range)
            .contains(&parameter);
        let value = property.scalar.as_ref().is_some_and(|scalar| {
            body.rendered
                .dependencies(scalar.range)
                .contains(&parameter)
        });
        if !key && !value {
            continue;
        }
        let Some(fact) = body.hir.field_fact_at(property.key_range) else {
            continue;
        };
        let context =
            hir::query::PropertyQueryContext::for_property(body.hir.properties(), property);
        for field in &fact.fields {
            let field = ir.field(*field);
            if key {
                sources.extend(ir_semantic::projected_source(
                    ir,
                    field.key,
                    &property.key,
                    snapshot,
                    &context,
                ));
            }
            if value
                && let (rules::ir::FieldValue::Scalar(matcher), Some(scalar)) =
                    (field.value, &property.scalar)
            {
                sources.extend(ir_semantic::projected_source(
                    ir,
                    matcher,
                    &scalar.value,
                    snapshot,
                    &context,
                ));
            }
        }
    }
    Ok(sources.into_iter().collect())
}

fn body_interpretations(
    body: &BodyAnalysis,
    parameter: &str,
) -> Vec<crate::types::TemplateInterpretation> {
    body.hir
        .overload_facts()
        .iter()
        .filter(|fact| {
            body.rendered
                .dependencies(fact.container)
                .contains(&parameter.to_ascii_lowercase())
        })
        .map(|fact| crate::types::TemplateInterpretation {
            schema: fact.schema.index(),
            fields: fact.fields.iter().map(|field| field.index()).collect(),
            container: fact.container,
            conditional: fact.validation != hir::analysis::Validation::Valid,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) struct ParameterSite {
    pub(crate) domain: Domain,
    pub(crate) token: TemplateToken,
    pub(crate) state: ScopeState,
    pub(crate) query_context: hir::query::OwnedQueryContext,
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
        matchers.iter().any(|matcher| {
            candidate_matches(
                snapshot,
                *matcher,
                &rendered,
                &self.state,
                &self.query_context,
            )
        })
    }
    pub(crate) fn accepts(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        value: &str,
    ) -> bool {
        if value.contains('$') {
            return true;
        }
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
                    &self.query_context,
                    affixes(&self.token, parameter)
                        .is_some_and(|(head, tail)| !head.is_empty() || !tail.is_empty()),
                )
            }),
            Domain::Key { schema, shape } => ir.lookup(*schema, &rendered, *shape).any(|id| {
                field_allowed(snapshot, id, &self.state)
                    && ir_semantic::matcher_matches_with_context(
                        ir,
                        ir.field(id).key,
                        &rendered,
                        &WorkspaceFacts { snapshot },
                        &self.state,
                        &self.query_context,
                    )
            }),
            Domain::Template { .. } | Domain::Unresolved => true,
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
                    ir_semantic::spellings_with_context(
                        ir,
                        *matcher,
                        snapshot,
                        rendered_prefix,
                        Some(&self.state),
                        &self.query_context,
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
                    ir_semantic::spellings_with_context(
                        ir,
                        ir.field(id).key,
                        snapshot,
                        rendered_prefix,
                        Some(&self.state),
                        &self.query_context,
                    )
                })
                .collect(),
            Domain::Template { schema, .. } => {
                ir.schema(*schema).items.map_or_else(Vec::new, |matcher| {
                    ir_semantic::spellings_with_context(
                        ir,
                        matcher,
                        snapshot,
                        rendered_prefix,
                        Some(&self.state),
                        &self.query_context,
                    )
                })
            }
            Domain::Unresolved => Vec::new(),
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
                    match hir::checking::scalar_validation_with_context(
                        snapshot.ir(),
                        *matcher,
                        &rendered,
                        &self.state,
                        &WorkspaceFacts { snapshot },
                        &self.query_context,
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
                    match hir::checking::scalar_outcome_with_context(
                        snapshot.ir(),
                        snapshot.ir().field(id).key,
                        &rendered,
                        &WorkspaceFacts { snapshot },
                        &self.query_context,
                    )
                    .0
                    {
                        Some(true) => {}
                        None => {
                            unknown = true;
                            continue;
                        }
                        Some(false) => continue,
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
            Domain::Template { .. } | Domain::Unresolved => Validation::Unknown,
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
    context: &impl rules::query::QueryContext,
) -> bool {
    let ir = snapshot.ir();
    match ir.matcher(matcher) {
        Matcher::Loc => crate::semantic::workspace_member(snapshot, "localisation", value),
        Matcher::Union(items) => items
            .iter()
            .any(|item| candidate_matches(snapshot, *item, value, state, context)),
        _ => ir_semantic::matcher_matches_with_context(
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
            context,
        ),
    }
}

fn rendered_value_matches(
    snapshot: &AnalysisSnapshot,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
    context: &impl rules::query::QueryContext,
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
                || ir_semantic::matcher_matches_with_context(
                    ir,
                    matcher,
                    value,
                    &WorkspaceFacts { snapshot },
                    state,
                    context,
                )
        }
        Matcher::Union(items) => items
            .iter()
            .any(|item| rendered_value_matches(snapshot, *item, value, state, context, affixed)),
        _ => ir_semantic::matcher_matches_with_context(
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
            context,
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
            domain: site.domain,
            token: site.token,
            state: site.state,
            query_context: site.query_context,
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

pub(crate) fn invocation_state(hir: &hir::HirFile, invocation: &hir::HirProperty) -> ScopeState {
    hir.scope_fact_at(invocation.key_range)
        .map(|fact| fact.state.clone())
        .unwrap_or(ScopeState {
            root: hir::ScopeValue::Unknown,
            current: vec![hir::ScopeValue::Unknown],
            from: Vec::new(),
            previous: Vec::new(),
        })
}

/// One exact position of a root argument in its complete instantiated parent.
pub(crate) struct ConsumedPosition {
    pub(crate) generated: u32,
    pub(crate) piece: usize,
}
pub(crate) struct ConsumptionProjection {
    pub(crate) body: std::sync::Arc<BodyAnalysis>,
    pub(crate) positions: Vec<ConsumedPosition>,
    pub(crate) parameter: String,
    pub(crate) invocation: text::TextRange,
    argument: text::TextRange,
    scalar: Option<hir::HirScalar>,
    quote_map: Option<parser::QuotedScriptSourceMap>,
    insertion: u32,
}
impl ConsumptionProjection {
    pub(crate) fn edit(
        &self,
        point: &ConsumedPosition,
        range: text::TextRange,
        text: &str,
        snippet: bool,
    ) -> Option<(text::TextRange, String)> {
        let piece = self.body.rendered.pieces.get(point.piece)?;
        if range.start() < piece.range.start() || range.end() > piece.range.end() {
            return None;
        }
        let map = piece.binding_source.as_ref()?;
        let (start, end) = (
            *map.offsets
                .get((range.start() - piece.range.start()) as usize)?,
            *map.offsets
                .get((range.end() - piece.range.start()) as usize)?,
        );
        let relative = text::TextRange::new(start, end)?;
        let range = if let Some(scalar) = &self.scalar {
            let relative = self
                .quote_map
                .as_ref()
                .map_or(Some(relative), |map| map.decoded_range(relative))?;
            text::TextRange::new(
                scalar.range.start() + relative.start(),
                scalar.range.start() + relative.end(),
            )?
        } else {
            text::TextRange::empty(self.insertion)
        };
        let root_quoted = self.quote_map.is_some();
        let wrap = !root_quoted
            && (self.scalar.is_none()
                || text.chars().any(char::is_whitespace)
                || text.contains('{'));
        let mut encoded = text.to_owned();
        for _ in 0..map.quote_layers + usize::from(root_quoted || wrap) {
            encoded = crate::insertion::quoted_insertion(&encoded, snippet);
        }
        if wrap {
            encoded = format!("\"{encoded}\"");
        }
        Some((range, encoded))
    }
}

/// Locates script consumption using the shared instance and byte map, never by quoted-string guessing.
pub(crate) fn consumption_at(
    snapshot: &AnalysisSnapshot,
    input: &crate::support::ParsedInput,
    position: u32,
    cancellation: &CancellationToken,
) -> Result<Option<ConsumptionProjection>, Cancelled> {
    let Some(source) = input.hir.as_deref() else {
        return Ok(None);
    };
    for argument in source.properties().iter().filter(|argument| {
        argument.operator.is_some()
            && position > argument.key_range.end()
            && (argument.scalar.as_ref().is_some_and(|scalar| {
                scalar.range.start() <= position && position <= scalar.range.end()
            }) || argument.value_range.is_none()
                && argument.range.end() <= position
                && input
                    .source
                    .get(argument.range.end() as usize..position as usize)
                    .is_some_and(|gap| gap.trim().is_empty()))
    }) {
        let Some(invocation) = source
            .properties()
            .iter()
            .filter(|invocation| {
                invocation.path.len() + 1 == argument.path.len()
                    && argument.path.starts_with(&invocation.path)
                    && invocation.range.start() <= argument.range.start()
                    && argument.range.end() <= invocation.range.end()
            })
            .min_by_key(|invocation| invocation.range.len())
        else {
            continue;
        };
        let Some(field) = source.field_fact_at(invocation.key_range) else {
            continue;
        };
        let Some(kind) = field
            .fields
            .iter()
            .find_map(|id| template_kind(snapshot.ir(), snapshot.ir().field(*id).key))
        else {
            continue;
        };
        let Some(summary) =
            crate::semantic::dynamic_definition_summary(snapshot, &kind, &invocation.key)
        else {
            continue;
        };
        if !needs_body_analysis(snapshot, &summary) {
            continue;
        }
        let scalar = argument.scalar.clone();
        let quote_map = scalar
            .as_ref()
            .filter(|scalar| scalar.quoted)
            .and_then(|scalar| source.syntax().text(scalar.range))
            .and_then(parser::decode_quoted_script)
            .map(|(_, map)| map);
        let offset = scalar
            .as_ref()
            .map_or(0, |scalar| position.saturating_sub(scalar.range.start()));
        let offset = quote_map
            .as_ref()
            .map_or(Some(offset), |map| map.source_offset(offset));
        let Some(offset) = offset else {
            continue;
        };
        let override_value = scalar.is_none().then_some((argument.key.as_str(), ""));
        let Some(body) = analyse_body(snapshot, source, invocation, override_value, cancellation)?
        else {
            continue;
        };
        let mut positions = Vec::new();
        for (index, piece) in body
            .rendered
            .pieces
            .iter()
            .enumerate()
            .filter(|(_, piece)| piece.script)
        {
            let Some(map) = piece
                .binding_source
                .as_ref()
                .filter(|map| map.parameter.eq_ignore_ascii_case(&argument.key))
            else {
                continue;
            };
            let boundary = map
                .offsets
                .partition_point(|boundary| *boundary <= offset)
                .saturating_sub(1);
            if map.offsets.get(boundary) == Some(&offset) {
                positions.push(ConsumedPosition {
                    generated: piece.range.start() + boundary as u32,
                    piece: index,
                });
            }
        }
        if positions.is_empty() {
            for call in &body.rendered.calls {
                let Some((parameter, range)) = &call.source else {
                    continue;
                };
                if !parameter.eq_ignore_ascii_case(&argument.key)
                    || offset < range.start()
                    || range.end() < offset
                {
                    continue;
                }
                let view = &call.site.view;
                let Some((index, piece)) = view.pieces.iter().enumerate().find(|(_, piece)| {
                    piece.range.start() <= call.site.key_range.start()
                        && call.site.key_range.end() <= piece.range.end()
                        && piece.script
                }) else {
                    continue;
                };
                let Some(map) = &piece.binding_source else {
                    continue;
                };
                let boundary = map
                    .offsets
                    .partition_point(|boundary| *boundary <= offset)
                    .saturating_sub(1);
                if map.offsets.get(boundary) != Some(&offset) {
                    continue;
                }
                let parsed =
                    match parser::parse_script_bounded(&view.text, Default::default(), &mut || {
                        cancellation.checkpoint()
                    })? {
                        Ok(parsed) => std::sync::Arc::new(parsed),
                        Err(_) => continue,
                    };
                let hir = hir::lower_ir_schema_with_holes(
                    parsed,
                    snapshot.ir(),
                    call.site.schema,
                    Default::default(),
                    call.site.state.clone(),
                    &WorkspaceFacts { snapshot },
                    &view.trial_holes,
                );
                let shadow = std::sync::Arc::new(BodyAnalysis {
                    rendered: view.as_ref().clone(),
                    hir,
                    evidence: Vec::new(),
                    coverage: body.coverage.clone(),
                });
                return Ok(Some(ConsumptionProjection {
                    body: shadow,
                    positions: vec![ConsumedPosition {
                        generated: piece.range.start() + boundary as u32,
                        piece: index,
                    }],
                    parameter: argument.key.clone(),
                    invocation: invocation.key_range,
                    argument: argument.key_range,
                    scalar,
                    quote_map,
                    insertion: position,
                }));
            }
        }
        if !positions.is_empty() {
            return Ok(Some(ConsumptionProjection {
                body,
                positions,
                parameter: argument.key.clone(),
                invocation: invocation.key_range,
                argument: argument.key_range,
                scalar,
                quote_map,
                insertion: position,
            }));
        }
    }
    Ok(None)
}

/// Candidate trials reuse collision-checked marker prefixes. Their validation
/// still rebinds the exact argument and checks every original consumption site.
#[derive(Clone)]
pub(crate) struct ConsumptionEditValidator<'a> {
    snapshot: &'a AnalysisSnapshot,
    input: &'a crate::support::ParsedInput,
    projection: &'a ConsumptionProjection,
    trial_prefixes: BTreeMap<u32, String>,
    render_plan: Option<render_edit::RenderEdit<'a>>,
}

impl<'a> ConsumptionEditValidator<'a> {
    #[cfg(test)]
    pub(crate) fn has_render_plan(&self) -> bool {
        self.render_plan.is_some()
    }
    pub(crate) fn new(
        snapshot: &'a AnalysisSnapshot,
        input: &'a crate::support::ParsedInput,
        projection: &'a ConsumptionProjection,
    ) -> Self {
        Self {
            snapshot,
            input,
            projection,
            trial_prefixes: BTreeMap::new(),
            render_plan: render_edit::RenderEdit::new(snapshot, input, projection),
        }
    }

    pub(crate) fn validate(
        &mut self,
        item: &crate::CompletionItem,
        interpretations: &mut Vec<crate::types::TemplateInterpretation>,
        cancellation: &CancellationToken,
    ) -> Result<Analysis<hir::analysis::Validation>, Cancelled> {
        validate_consumption_edit(
            self.snapshot,
            self.input,
            self.projection,
            item,
            interpretations,
            &mut self.trial_prefixes,
            self.render_plan.as_mut(),
            cancellation,
            false,
        )
    }
    pub(crate) fn validate_with_known_frontier(
        &mut self,
        item: &crate::CompletionItem,
        interpretations: &mut Vec<crate::types::TemplateInterpretation>,
        cancellation: &CancellationToken,
    ) -> Result<Analysis<hir::analysis::Validation>, Cancelled> {
        if self.render_plan.as_mut().is_some_and(|plan| {
            plan.reject_encoded(
                self.snapshot,
                self.input,
                self.projection,
                item,
                self.trial_prefixes
                    .get(&item.replacement_range.start())
                    .map(String::as_str),
            )
        }) {
            return Ok(Analysis {
                value: hir::analysis::Validation::Invalid,
                coverage: Default::default(),
            });
        }
        let epoch = self
            .render_plan
            .as_ref()
            .map_or(0, |plan| plan.syntax_epoch());
        let checked = validate_consumption_edit(
            self.snapshot,
            self.input,
            self.projection,
            item,
            interpretations,
            &mut self.trial_prefixes,
            self.render_plan.as_mut(),
            cancellation,
            true,
        )?;
        if let Some(plan) = &mut self.render_plan
            && plan.syntax_epoch() != epoch
        {
            plan.remember_encoded(item);
        }
        Ok(checked)
    }
}

// A single closed quoted token cannot create a parse frontier or another
// property. Avoid building a throwaway CST before decoding this common carrier;
// malformed, unquoted and long edits retain the ordinary bounded parse.
fn closed_argument_value(raw: &str) -> Option<String> {
    if raw.len() > 64 * 1024 {
        return None;
    }
    let mut chars = raw.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    while let Some((offset, ch)) = chars.next() {
        if ch == '\\' {
            chars.next()?;
        } else if ch == '"' {
            return (offset + 1 == raw.len())
                .then(|| parser::decode_quoted_script(raw).map(|(value, _)| value))
                .flatten();
        }
    }
    None
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    #[test]
    fn closed_carrier_matches_full_parser_including_unicode_and_escapes() {
        let alphabet = ['a', '"', '\\', '\n', '中', '😀', '$', '{', '=', '}'];
        let mut seed = 17u64;
        for n in 0..4096 {
            let mut raw = String::from("\"");
            for _ in 0..n % 24 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                raw.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
            }
            raw.push('"');
            let Some(value) = closed_argument_value(&raw) else {
                continue;
            };
            let source = format!("value = {raw}");
            let parsed = parser::parse(parser::FileFormat::Script, &source);
            assert!(parsed.errors().is_empty(), "{raw:?}");
            let quoted = parsed
                .tokens()
                .iter()
                .find(|token| token.kind() == parser::TokenKind::Quoted)
                .unwrap();
            assert_eq!(parsed.text(quoted.range()), Some(raw.as_str()));
            assert_eq!(
                value,
                parser::decode_quoted_script(parsed.text(quoted.range()).unwrap())
                    .unwrap()
                    .0
            );
        }
        for raw in ["\"unterminated", "\"a\" extra", "not_quoted"] {
            assert!(closed_argument_value(raw).is_none());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_consumption_edit(
    snapshot: &AnalysisSnapshot,
    input: &crate::support::ParsedInput,
    projection: &ConsumptionProjection,
    item: &crate::CompletionItem,
    interpretations: &mut Vec<crate::types::TemplateInterpretation>,
    trial_prefixes: &mut BTreeMap<u32, String>,
    render_plan: Option<&mut render_edit::RenderEdit<'_>>,
    cancellation: &CancellationToken,
    known_incomplete: bool,
) -> Result<Analysis<hir::analysis::Validation>, Cancelled> {
    use hir::analysis::Validation;
    cancellation.checkpoint()?;
    let Some(original) = input.hir.as_deref() else {
        return Ok(Analysis {
            value: Validation::Unknown,
            coverage: Default::default(),
        });
    };
    let Some(invocation) = original.property_at_key_range(projection.invocation) else {
        return Ok(Analysis {
            value: Validation::Invalid,
            coverage: Default::default(),
        });
    };
    // Only the argument's token changes. Keep the caller's schema, scope,
    // siblings and other bindings rather than lowering the whole file for
    // every candidate. Nested carriers still pass through the same decoder
    // and Template instance validation below.
    let range = projection
        .scalar
        .as_ref()
        .map_or(text::TextRange::empty(projection.insertion), |scalar| {
            scalar.range
        });
    if item.replacement_range.start() < range.start() || item.replacement_range.end() > range.end()
    {
        return Ok(Analysis {
            value: Validation::Invalid,
            coverage: Default::default(),
        });
    }
    let mut raw = input.source_text(range).unwrap_or_default().to_owned();
    let prefix = trial_prefixes
        .entry(item.replacement_range.start())
        .or_insert_with(|| {
            let mut salt = 0;
            loop {
                let prefix = format!("__pdc_trial_{}_{}", item.replacement_range.start(), salt);
                if !input.source.contains(&prefix) {
                    break prefix;
                }
                salt += 1;
            }
        });
    let (text, markers) = if item.is_snippet {
        crate::insertion::snippet_trial(&item.insert_text, prefix)
    } else {
        (item.insert_text.clone(), Vec::new())
    };
    if input.source.len().saturating_add(text.len()) > parser::ScriptParseBudget::default().bytes {
        return Ok(Analysis {
            value: Validation::Unknown,
            coverage: hir::analysis::AnalysisCoverage {
                limits: BTreeSet::from([hir::analysis::AnalysisLimit::TextBytes]),
                residuals: Default::default(),
            },
        });
    }
    raw.replace_range(
        (item.replacement_range.start() - range.start()) as usize
            ..(item.replacement_range.end() - range.start()) as usize,
        &text,
    );
    let (raw, value) = if let Some(value) = closed_argument_value(&raw) {
        (raw, value)
    } else {
        let source = format!("value = {raw}");
        let parsed = match parser::parse_script_bounded(&source, Default::default(), &mut || {
            cancellation.checkpoint()
        })? {
            Ok(parsed) => parsed,
            Err(limit) => {
                return Ok(Analysis {
                    value: Validation::Unknown,
                    coverage: hir::analysis::AnalysisCoverage {
                        limits: BTreeSet::from([hir::template_text::parse_limit(limit)]),
                        residuals: Default::default(),
                    },
                });
            }
        };
        let scalar = parsed
            .root()
            .children()
            .find(|node| node.kind() == parser::CstKind::Property)
            .and_then(|node| {
                node.children()
                    .find(|node| node.kind() == parser::CstKind::Value)
            })
            .and_then(|node| {
                node.children().find(|node| {
                    matches!(
                        node.kind(),
                        parser::CstKind::BareValue | parser::CstKind::QuotedString
                    )
                })
            });
        let Some(scalar) = scalar else {
            return Ok(Analysis {
                value: Validation::Unknown,
                coverage: Default::default(),
            });
        };
        let raw = parsed.text(scalar.range()).unwrap_or_default().to_owned();
        let value =
            parser::decode_quoted_script(&raw).map_or_else(|| raw.clone(), |(value, _)| value);
        (raw, value)
    };
    let edit = ArgumentEdit {
        key_range: projection.argument,
        value,
        raw,
    };
    let reused = if let Some(plan) = render_plan {
        plan.validate(
            snapshot,
            &edit,
            &item.label,
            &markers,
            cancellation,
            known_incomplete,
        )?
    } else {
        None
    };
    let result = if reused.is_some() {
        reused
    } else {
        analyse_body_with_argument_edit(
            snapshot,
            original,
            invocation,
            &BTreeMap::new(),
            &markers,
            Some(&edit),
            cancellation,
            known_incomplete.then_some(projection.parameter.as_str()),
        )?
    };
    let body = match result {
        Some(render_edit::RenderValidation::Rejected(coverage)) => {
            return Ok(Analysis {
                value: Validation::Invalid,
                coverage,
            });
        }
        Some(render_edit::RenderValidation::Body(body)) => body,
        None => {
            return Ok(Analysis {
                value: Validation::Unknown,
                coverage: Default::default(),
            });
        }
    };
    *interpretations = body_interpretations(&body, &projection.parameter);
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
            evidence_dependencies(&body, evidence)
                .contains(&projection.parameter.to_ascii_lowercase())
        });
    Ok(Analysis {
        value: if rejected {
            Validation::Invalid
        } else if markers.is_empty() && body.coverage.is_known() {
            Validation::Valid
        } else {
            Validation::Unknown
        },
        coverage: body.coverage.clone(),
    })
}

/// Visits actual script-consuming instances for navigation, coloring and hints.
pub(crate) fn for_each_consumption(
    snapshot: &AnalysisSnapshot,
    input: &crate::support::ParsedInput,
    cancellation: &CancellationToken,
    visit: &mut impl FnMut(&hir::HirFile, &hir::HirProperty, &BodyAnalysis) -> Result<(), Cancelled>,
) -> Result<(), Cancelled> {
    let Some(source) = input.hir.as_deref() else {
        return Ok(());
    };
    for invocation in source.properties() {
        cancellation.checkpoint()?;
        let Some(fact) = source.field_fact_at(invocation.key_range) else {
            continue;
        };
        let Some(kind) = fact
            .fields
            .iter()
            .find_map(|id| template_kind(snapshot.ir(), snapshot.ir().field(*id).key))
        else {
            continue;
        };
        if source.parameter_references().iter().any(|r| {
            invocation.range.start() <= r.range.start() && r.range.end() <= invocation.range.end()
        }) || source.definitions().iter().any(|d| {
            d.kind.eq_ignore_ascii_case(&kind) && d.selection_range == invocation.key_range
        }) {
            continue;
        }
        let Some(summary) =
            crate::semantic::dynamic_definition_summary(snapshot, &kind, &invocation.key)
        else {
            continue;
        };
        if !needs_body_analysis(snapshot, &summary) {
            continue;
        }
        if let Some(body) = analyse_body(snapshot, source, invocation, None, cancellation)? {
            visit(source, invocation, &body)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod encoded_rejection_tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn encoded_preflight_matches_full_proofs_and_keeps_range_marker_and_budget_guards() {
        let mut host = engine::AnalysisHost::with_ir(
            game::eu4::runtime_rules().unwrap(),
            game::eu4::profile(),
            game::eu4::first_party_ir().unwrap(),
        );
        host.open_document(engine::DocumentId::new("file:///encoded/common/scripted_effects/wrappers.txt"), 1,
            "prefix_shared={set_variable={which=counter value=1} while={limit={check_variable={which=counter value=1}} $BODY$ subtract_variable={which=counter value=1}}}".into(), None).unwrap();
        let source = "# 中文😀\r\ncountry_event={immediate={prefix_shared={BODY=\"random_owned_province={limit={religion=shiite} @={type=religious_rebels size=1}}\"}}}";
        let position = source.find('@').unwrap() as u32;
        let id = engine::DocumentId::new("file:///encoded/events/sample.txt");
        host.open_document(id.clone(), 1, source.replace('@', ""), None)
            .unwrap();
        let snapshot = host.snapshot();
        let input = crate::support::input_for_document(&snapshot, &id).unwrap();
        let projection = consumption_at(&snapshot, &input, position, &CancellationToken::new())
            .unwrap()
            .unwrap();
        let point = &projection.positions[0];
        let item = |label: &str, snippet: &str| {
            let generated_range =
                crate::support::word_range(&projection.body.rendered.text, point.generated);
            let (range, text) = projection
                .edit(point, generated_range, snippet, true)
                .unwrap();
            crate::CompletionItem {
                label: label.into(),
                insert_text: text,
                replacement_range: range,
                is_snippet: true,
                kind: crate::CompletionKind::Key,
                detail: String::new(),
                documentation: None,
                sort_score: 0,
                deprecated: false,
                resolve_data: None,
                template_evidence: None,
            }
        };
        let mut validator = ConsumptionEditValidator::new(&snapshot, &input, &projection);
        assert!(validator.render_plan.is_some());
        let reference = |candidate: &crate::CompletionItem| {
            with_full_template_rendering(|| {
                ConsumptionEditValidator::new(&snapshot, &input, &projection)
                    .validate(candidate, &mut Vec::new(), &CancellationToken::new())
                    .unwrap()
            })
        };
        for label in ["add_base_tax", "add_base_manpower", "add_base_production"] {
            let candidate = item(label, &format!("{label} = {{\n\t$0\n}}"));
            let result = validator
                .validate_with_known_frontier(
                    &candidate,
                    &mut Vec::new(),
                    &CancellationToken::new(),
                )
                .unwrap();
            assert_eq!(result.value, reference(&candidate).value, "{label}");
            assert_eq!(result.value, hir::analysis::Validation::Invalid);
        }
        let hits = validator.render_plan.as_ref().unwrap().encoded_hits();
        assert!(
            hits > 0,
            "the third identical carrier shape must skip decoding"
        );
        let mut changed = item("add_base_tax", "add_base_tax = $0");
        let result = validator
            .validate_with_known_frontier(&changed, &mut Vec::new(), &CancellationToken::new())
            .unwrap();
        assert_eq!(result.value, reference(&changed).value);
        assert_eq!(validator.render_plan.as_ref().unwrap().encoded_hits(), hits);
        changed = item("add_base_tax", "add_base_tax = {\n\t$0\n}");
        changed.replacement_range = text::TextRange::new(
            changed.replacement_range.start(),
            changed.replacement_range.end() + 1,
        )
        .unwrap();
        let result = validator
            .validate_with_known_frontier(&changed, &mut Vec::new(), &CancellationToken::new())
            .unwrap();
        assert_eq!(result.value, reference(&changed).value);
        assert_eq!(validator.render_plan.as_ref().unwrap().encoded_hits(), hits);
        let candidate = item("add_base_tax", "add_base_tax = {\n\t$0\n}");
        let prefix = validator
            .trial_prefixes
            .get(&candidate.replacement_range.start())
            .unwrap()
            .clone();
        let plan = validator.render_plan.as_mut().unwrap();
        let mut near_budget = input.clone();
        near_budget.source = Arc::from("x".repeat(parser::ScriptParseBudget::default().bytes));
        assert!(!plan.reject_encoded(
            &snapshot,
            &near_budget,
            &projection,
            &candidate,
            Some(&prefix)
        ));
        let label = format!("{prefix}_0");
        let collision = item(&label, &format!("{label} = {{\n\t$0\n}}"));
        assert!(!plan.reject_encoded(&snapshot, &input, &projection, &collision, Some(&prefix)));
    }
}
