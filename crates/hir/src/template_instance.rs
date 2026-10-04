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
    Ok(TemplateInstance {
        rendered,
        hir,
        evidence,
        coverage,
    })
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
