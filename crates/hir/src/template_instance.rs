//! Whole-container Template specialization shared by indexing and IDE projections.
use crate::analysis::AnalysisCoverage;
use crate::{HirFile, HirProperty, ScopeState};
use rules::ir::{RulesIr, SchemaId, SymbolFacts};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Query coverage: fact discovery needs correlated lowering and source maps;
/// diagnostics additionally checks complete-container rejection evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstanceGoal {
    Facts,
    Validation,
}

/// One immutable instance, including all fixed and inserted siblings.
#[derive(Clone)]
pub struct TemplateInstance {
    pub rendered: crate::template_text::RenderedTemplate,
    pub hir: HirFile,
    pub evidence: Vec<crate::checking::ConstraintEvidence>,
    pub coverage: AnalysisCoverage,
}

#[allow(clippy::too_many_arguments)]
pub fn instantiate<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    template: Arc<crate::Template>,
    bindings: &BTreeMap<String, String>,
    raw: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    goal: InstanceGoal,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<TemplateInstance, E> {
    checkpoint()?;
    let memo = markers
        .is_empty()
        .then(|| facts.shared_template_memo())
        .flatten();
    let Some(memo) = memo else {
        return instantiate_uncached(
            ir, facts, template, bindings, raw, present, schema, state, markers, goal, checkpoint,
        );
    };
    let key = format!(
        "instance-facts:{:p}:{schema:?}:{state:?}:{bindings:?}:{raw:?}:{present:?}",
        Arc::as_ptr(&template.program)
    );
    let previous = memo.get::<CachedInstances>(&key);
    if let Some(previous) = &previous {
        for cached in &previous.0 {
            if cached._owner == template && cached.reads.validate_and_replay(facts, checkpoint)? {
                let mut instance = cached.instance.clone();
                if goal == InstanceGoal::Validation {
                    // Expansion and semantic lowering are identical for both
                    // goals. Only constraints add validation evidence/reads.
                    let checked = crate::checking::check_fragment(
                        ir,
                        &instance.hir,
                        facts,
                        &instance.rendered,
                        checkpoint,
                    )?;
                    instance.coverage.merge(&checked.coverage);
                    instance.evidence = checked.value;
                }
                return Ok(instance);
            }
        }
    }
    if goal == InstanceGoal::Validation {
        return instantiate_uncached(
            ir, facts, template, bindings, raw, present, schema, state, markers, goal, checkpoint,
        );
    }
    let recording = crate::memo_facts::RecordingFacts::new(facts);
    let instance = instantiate_uncached(
        ir,
        &recording,
        template.clone(),
        bindings,
        raw,
        present,
        schema,
        state,
        markers,
        goal,
        checkpoint,
    )?;
    let reads = recording.finish();
    checkpoint()?;
    if instance.coverage.is_complete() {
        let bytes = reads
            .bytes()
            .saturating_add(template.source.len().saturating_mul(128))
            .saturating_add(instance.rendered.text.len().saturating_mul(8))
            .saturating_add(instance.rendered.pieces.len().saturating_mul(512))
            .saturating_add(
                instance
                    .hir
                    .syntax()
                    .tree()
                    .node_count()
                    .saturating_mul(512),
            )
            .saturating_add(instance.hir.properties().len().saturating_mul(2048));
        let mut entries = previous.map_or_else(Vec::new, |previous| previous.0.clone());
        if entries.len() >= 4 {
            entries.remove(0);
        }
        entries.push(Arc::new(CachedInstance {
            _owner: template,
            instance: instance.clone(),
            reads,
            bytes,
        }));
        let bytes = entries
            .iter()
            .fold(0usize, |sum, entry| sum.saturating_add(entry.bytes));
        memo.replace(key, Arc::new(CachedInstances(entries)), bytes);
    }
    Ok(instance)
}

struct CachedInstance {
    // Keep the program's pointer identity alive while its entry exists.
    _owner: Arc<crate::Template>,
    instance: TemplateInstance,
    reads: crate::memo_facts::Receipts,
    bytes: usize,
}
struct CachedInstances(Vec<Arc<CachedInstance>>);

/// Checks an incrementally rendered container with the ordinary full lowering
/// and constraint checker. The caller must prove the text rendering reusable;
/// a possible call or parse frontier requires the normal expansion path.
#[allow(clippy::too_many_arguments)]
pub fn instantiate_rendered_no_calls<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    rendered: crate::template_text::RenderedTemplate,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    can_call: &mut impl FnMut(&str) -> bool,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<TemplateInstance>, E> {
    instantiate_rendered_with_prefix(
        ir, facts, rendered, schema, state, markers, can_call, checkpoint, &mut None,
    )
}

/// Query-local validation view. Prefix entries never cross facts or rules views.
#[derive(Clone)]
pub struct FragmentSession<'a, F> {
    ir: &'a RulesIr,
    facts: F,
    prefix: Option<crate::ir_lowering::PrefixMemo>,
}
impl<'a, F: SymbolFacts> FragmentSession<'a, F> {
    /// Starts an isolated batch against one immutable fact view and source prefix.
    pub fn new(ir: &'a RulesIr, facts: F, source: Arc<str>) -> Self {
        Self {
            ir,
            facts,
            prefix: Some(crate::ir_lowering::PrefixMemo::new(source)),
        }
    }
    /// Number of closed properties reused within this immutable batch.
    pub fn reused_properties(&self) -> usize {
        self.prefix.as_ref().map_or(0, |memo| memo.hits)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn instantiate<E>(
        &mut self,
        rendered: crate::template_text::RenderedTemplate,
        schema: SchemaId,
        state: ScopeState,
        markers: &[String],
        can_call: &mut impl FnMut(&str) -> bool,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<TemplateInstance>, E> {
        if let Some(memo) = &mut self.prefix {
            memo.begin(&rendered.text);
        }
        instantiate_rendered_with_prefix(
            self.ir,
            &self.facts,
            rendered,
            schema,
            state,
            markers,
            can_call,
            checkpoint,
            &mut self.prefix,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn instantiate_rendered_with_prefix<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    mut rendered: crate::template_text::RenderedTemplate,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    can_call: &mut impl FnMut(&str) -> bool,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
    prefix_memo: &mut Option<crate::ir_lowering::PrefixMemo>,
) -> Result<Option<TemplateInstance>, E> {
    checkpoint()?;
    for marker in markers {
        for (start, _) in rendered.text.match_indices(marker) {
            if let Some(range) = text::TextRange::new(start as u32, (start + marker.len()) as u32) {
                rendered.trial_holes.push(range);
            }
        }
    }
    let progress =
        parser::parse_script_prefix_bounded(&rendered.text, Default::default(), checkpoint)?;
    if progress.frontier.is_some() {
        return Ok(None);
    }
    let Some(parsed) = progress.parsed else {
        return Ok(None);
    };
    let parsed = Arc::new(parsed);
    let collected = crate::collector::collect(&parsed);
    for property in collected.properties.iter() {
        checkpoint()?;
        if can_call(&property.key) {
            return Ok(None);
        }
    }
    let hir = crate::lower_ir_schema_collected_with_memo(
        parsed,
        ir,
        schema,
        Default::default(),
        state,
        facts,
        None,
        &rendered.trial_holes,
        collected,
        prefix_memo,
    );
    let checked = crate::checking::check_fragment(ir, &hir, facts, &rendered, checkpoint)?;
    let mut coverage = rendered.coverage.clone();
    coverage.merge(hir.analysis_coverage());
    coverage.merge(&checked.coverage);
    Ok(Some(TemplateInstance {
        rendered,
        hir,
        evidence: checked.value,
        coverage,
    }))
}

#[allow(clippy::too_many_arguments)]
fn instantiate_uncached<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    template: Arc<crate::Template>,
    bindings: &BTreeMap<String, String>,
    raw: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    goal: InstanceGoal,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<TemplateInstance, E> {
    match instantiate_validation(
        ir, facts, template, bindings, raw, present, schema, state, markers, goal, None, checkpoint,
    )? {
        Some(body) => Ok(body),
        None => unreachable!("ordinary validation always materializes its instance"),
    }
}

/// Materializes the ordinary expansion, but may stop before semantic lowering
/// when a physical syntax error already proves rejection by this binding.
/// Callers using this goal must already have an actual incomplete frontier;
/// otherwise they still need the complete traversal's frontier enumeration.
#[allow(clippy::too_many_arguments)]
pub fn instantiate_until_syntax_rejection<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    template: Arc<crate::Template>,
    bindings: &BTreeMap<String, String>,
    raw: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    parameter: &str,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<TemplateInstance>, E> {
    instantiate_validation(
        ir,
        facts,
        template,
        bindings,
        raw,
        present,
        schema,
        state,
        markers,
        InstanceGoal::Validation,
        Some(parameter),
        checkpoint,
    )
}

#[allow(clippy::too_many_arguments)]
fn instantiate_validation<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    template: Arc<crate::Template>,
    bindings: &BTreeMap<String, String>,
    raw: &BTreeMap<String, String>,
    present: &BTreeSet<String>,
    schema: SchemaId,
    state: ScopeState,
    markers: &[String],
    goal: InstanceGoal,
    reject_parameter: Option<&str>,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<TemplateInstance>, E> {
    let mut rendered = crate::template_text::render_expanded(
        ir,
        facts,
        template,
        bindings,
        raw,
        present,
        schema,
        state.clone(),
        1024 * 1024,
        checkpoint,
    )?;
    for marker in markers {
        for (start, _) in rendered.text.match_indices(marker) {
            if let Some(range) = text::TextRange::new(start as u32, (start + marker.len()) as u32) {
                rendered.trial_holes.push(range);
            }
        }
    }
    let progress =
        parser::parse_script_prefix_bounded(&rendered.text, Default::default(), checkpoint)?;
    if let Some((limit, frontier)) = progress.frontier {
        rendered
            .coverage
            .limits
            .insert(crate::template_text::parse_limit(limit));
        rendered.frontiers.push(frontier);
        if let Some(range) = text::TextRange::new(frontier, rendered.text.len() as u32) {
            rendered.trial_holes.push(range);
        }
    }
    let parsed = progress
        .parsed
        .unwrap_or_else(|| parser::parse(parser::FileFormat::Script, ""));
    if let Some(parameter) = reject_parameter
        && parsed.errors().iter().any(|error| {
            !rendered.has_hole(error.range)
                && !rendered.has_uncertain_text(error.range)
                && !rendered.has_unresolved_structure(error.range)
                && rendered.dependencies(error.range).contains(parameter)
        })
    {
        return Ok(None);
    }
    let hir = crate::lower_ir_schema_with_holes(
        Arc::new(parsed),
        ir,
        schema,
        Default::default(),
        state,
        facts,
        &rendered.trial_holes,
    );
    let mut coverage = rendered.coverage.clone();
    coverage.merge(hir.analysis_coverage());
    let evidence = if goal == InstanceGoal::Facts {
        Vec::new()
    } else {
        let checked = crate::checking::check_fragment(ir, &hir, facts, &rendered, checkpoint)?;
        coverage.merge(&checked.coverage);
        checked.value
    };
    Ok(Some(TemplateInstance {
        rendered,
        hir,
        evidence,
        coverage,
    }))
}

/// Exact generated-to-root binding projection. A token spanning several root
/// bindings has no inverse range and cannot be turned into a byte edit.
pub fn project_source_range<'a>(
    rendered: &crate::template_text::RenderedTemplate,
    range: text::TextRange,
    source: &parser::ParsedFile,
    arguments: impl DoubleEndedIterator<Item = &'a HirProperty> + Clone,
) -> Option<text::TextRange> {
    let (parameter, relative) = crate::template_text::binding_range(rendered, range)?;
    project_binding_range(&parameter, relative, source, arguments)
}

/// Projects a previously consumed call edge without inventing a generated range.
pub fn project_binding_range<'a>(
    parameter: &str,
    relative: text::TextRange,
    source: &parser::ParsedFile,
    arguments: impl DoubleEndedIterator<Item = &'a HirProperty> + Clone,
) -> Option<text::TextRange> {
    let scalar = arguments
        .rev()
        .find(|arg| arg.key.eq_ignore_ascii_case(parameter) && arg.scalar.is_some())?
        .scalar
        .as_ref()?;
    let relative = if scalar.quoted {
        parser::decode_quoted_script(source.text(scalar.range)?)?
            .1
            .decoded_range(relative)?
    } else {
        relative
    };
    text::TextRange::new(
        scalar.range.start().checked_add(relative.start())?,
        scalar.range.start().checked_add(relative.end())?,
    )
}

#[cfg(test)]
mod prefix_tests {
    use super::*;
    #[test]
    fn shared_closed_prefix_keeps_every_fact_constraint_and_frontier() {
        let ir = game::eu4::first_party_ir().unwrap();
        let schema =
            crate::template::template_body(&ir, ir.type_by_name("scripted_effect").unwrap())
                .unwrap();
        let base = "# 中文😀\r\nset_variable = { which = ForLoopCounter value = 1 } while = { limit = { check_variable = { which = ForLoopCounter value = 1 } } random_owned_province = { limit = { religion = shiite } BODY } subtract_variable = { which = ForLoopCounter value = 1 } }";
        let facts = rules::ir::NoSymbolFacts;
        let mut session = FragmentSession::new(&ir, facts, Arc::from(base));
        for state in [
            ScopeState::initial(crate::ScopeValue::known_single("country")),
            ScopeState::initial(crate::ScopeValue::known_single("province")),
            ScopeState::initial(crate::ScopeValue::Unknown),
        ] {
            for replacement in [
                "add_base_tax = 1",
                "add_base_manpower = 2",
                "missing = { __hole } = { size = 1 }",
                "spawn_rebels = { type = religious_rebels size = 1 }",
                "bad = {",
                "add_stability = 1",
            ] {
                let rendered = crate::template_text::RenderedTemplate {
                    text: base.replace("BODY", replacement),
                    ..Default::default()
                };
                let markers = vec!["__hole".to_owned()];
                let actual = session
                    .instantiate(
                        rendered.clone(),
                        schema,
                        state.clone(),
                        &markers,
                        &mut |_| false,
                        &mut || Ok::<(), ()>(()),
                    )
                    .unwrap()
                    .unwrap();
                let reference = instantiate_rendered_no_calls(
                    &ir,
                    &facts,
                    rendered,
                    schema,
                    state.clone(),
                    &markers,
                    &mut |_| false,
                    &mut || Ok::<(), ()>(()),
                )
                .unwrap()
                .unwrap();
                assert_eq!(actual.hir, reference.hir, "{replacement}");
                assert_eq!(actual.coverage, reference.coverage, "{replacement}");
                let evidence = |instance: &TemplateInstance| {
                    instance
                        .evidence
                        .iter()
                        .map(|e| {
                            (
                                e.kind,
                                e.severity,
                                e.range,
                                e.container,
                                e.explanation.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(evidence(&actual), evidence(&reference), "{replacement}");
            }
        }
        assert!(session.reused_properties() > 10);
    }
}

#[cfg(test)]
mod shared_goal_tests {
    use super::*;
    use std::cell::Cell;
    struct Facts {
        enabled: Cell<bool>,
        memo: Arc<rules::template::TemplateMemo>,
        shared: bool,
    }
    impl SymbolFacts for Facts {
        fn shared_template_memo(&self) -> Option<Arc<rules::template::TemplateMemo>> {
            self.shared.then(|| self.memo.clone())
        }
        fn type_member(&self, _: rules::ir::TypeId, _: &str) -> bool {
            self.enabled.get()
        }
    }
    #[test]
    fn validation_reuses_fact_expansion_but_keeps_all_constraints_and_invalidation() {
        let ir = game::eu4::first_party_ir().unwrap();
        let source = "sample = { add_prestige = nope has_country_flag = missing }";
        let definitions = crate::lower_shared_with_ir(
            Arc::new(parser::parse(parser::FileFormat::Script, source)),
            &text::LogicalPath::parse("common/scripted_effects/test.txt").unwrap(),
            &game::eu4::runtime_rules().unwrap(),
            &game::eu4::profile(),
            &ir,
        );
        let template = Arc::new(definitions.dynamic_templates()[0].clone());
        let schema =
            crate::template::template_body(&ir, ir.type_by_name("scripted_effect").unwrap())
                .unwrap();
        let memo = Arc::new(rules::template::TemplateMemo::with_max_bytes(
            64 * 1024 * 1024,
        ));
        let mut facts = Facts {
            enabled: Cell::new(false),
            memo,
            shared: true,
        };
        let run = |facts: &Facts, goal| {
            instantiate(
                &ir,
                facts,
                template.clone(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeSet::new(),
                schema,
                ScopeState::initial(crate::ScopeValue::known_single("country")),
                &[],
                goal,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
        };
        let base = run(&facts, InstanceGoal::Facts);
        assert!(base.evidence.is_empty());
        for enabled in [false, true, false] {
            facts.enabled.set(enabled);
            let shared = run(&facts, InstanceGoal::Validation);
            facts.shared = false;
            let fresh = run(&facts, InstanceGoal::Validation);
            assert_eq!(shared.hir, fresh.hir);
            assert_eq!(shared.coverage, fresh.coverage);
            assert_eq!(
                format!("{:?}", shared.evidence),
                format!("{:?}", fresh.evidence)
            );
            assert!(
                !shared.evidence.is_empty(),
                "bad prestige value must still be checked"
            );
            facts.shared = true;
            assert!(run(&facts, InstanceGoal::Facts).evidence.is_empty());
        }
    }
}
