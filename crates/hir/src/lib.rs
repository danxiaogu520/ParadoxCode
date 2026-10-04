//! Rule-aware, game-independent semantic lowering boundary.
//!
//! Template contracts and query responsibilities are documented in `crates/hir/TEMPLATES.md`.

use std::sync::Arc;

use parser::ParsedFile;
use rules::{GameProfile, RuleSet};
use text::LogicalPath;

pub mod analysis;
pub mod block_checking;
pub mod checking;
mod collector;
mod ir_lowering;
mod model;
mod parameters;
mod scope;
pub mod template;
pub mod template_instance;
mod template_lowering;
pub mod template_relations;
pub mod template_scope;
pub mod template_text;

pub use model::*;
fn range_within(inner: text::TextRange, outer: text::TextRange) -> bool {
    inner.start() >= outer.start() && inner.end() <= outer.end()
}

/// Lowers a parsed PDX file into game-independent structural facts.
#[must_use]
pub fn lower(syntax: ParsedFile, rules: &RuleSet) -> HirFile {
    lower_shared(Arc::new(syntax), rules)
}

/// Lowers a shared parsed file without copying its CST.
#[must_use]
pub fn lower_shared(syntax: Arc<ParsedFile>, rules: &RuleSet) -> HirFile {
    lower_shared_impl(syntax, None, rules, None, None, None, true)
}

/// Lowers a parsed file with an explicitly selected game profile and logical path.
#[must_use]
pub fn lower_with_profile(
    syntax: ParsedFile,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
) -> HirFile {
    lower_shared_with_profile(Arc::new(syntax), logical_path, rules, profile)
}

/// Lowers a shared parsed file with profile-aware semantic interpretation.
#[must_use]
pub fn lower_shared_with_profile(
    syntax: Arc<ParsedFile>,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
) -> HirFile {
    lower_shared_impl(
        syntax,
        Some(logical_path),
        rules,
        Some(profile),
        None,
        None,
        true,
    )
}

/// Lowers a shared parsed file directly from the compiled Rules IR.
#[must_use]
pub fn lower_shared_with_ir(
    syntax: Arc<ParsedFile>,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &rules::ir::RulesIr,
) -> HirFile {
    lower_shared_impl(
        syntax,
        Some(logical_path),
        rules,
        Some(profile),
        Some(ir),
        None,
        true,
    )
}

/// IR lowering with workspace facts for symbol references and subtype predicates.
#[must_use]
pub fn lower_shared_with_ir_and_facts(
    syntax: Arc<ParsedFile>,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
) -> HirFile {
    lower_shared_impl(
        syntax,
        Some(logical_path),
        rules,
        Some(profile),
        Some(ir),
        Some(facts),
        true,
    )
}

/// Lowers the facts consumed by disk indexing, without validation-only tables.
/// The frontend must be discarded after extracting its shard; semantic queries
/// use the full lowering functions above.
#[must_use]
pub fn lower_shared_for_index_with_ir(
    syntax: Arc<ParsedFile>,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &rules::ir::RulesIr,
    facts: Option<&dyn rules::ir::SymbolFacts>,
) -> HirFile {
    lower_shared_impl(
        syntax,
        Some(logical_path),
        rules,
        Some(profile),
        Some(ir),
        facts,
        false,
    )
}

/// Applies the same declared scope transition used by the IR source walker.
#[must_use]
pub fn transition_ir_scope(
    ir: &rules::ir::RulesIr,
    state: ScopeState,
    effect: Option<&rules::ir::ScopeEffect>,
    key: &str,
) -> ScopeState {
    ir_lowering::transition_state(ir, state, effect, key)
}

/// Recognizes a declared register chain or scope-link pattern.
#[must_use]
pub fn is_ir_scope_link(ir: &rules::ir::RulesIr, key: &str) -> bool {
    ir_lowering::link_or_register_matches(ir, key)
}

/// Reads the state slot selected by a declared register spelling or chain.
#[must_use]
pub fn ir_scope_register_value<'a>(
    ir: &rules::ir::RulesIr,
    state: &'a ScopeState,
    key: &str,
) -> Option<&'a ScopeValue> {
    let (register, depth) = ir.scopes.register(ir.strings(), key)?;
    match register.role {
        rules::source::RegisterRole::Root => Some(&state.root),
        rules::source::RegisterRole::Current => state.current.first(),
        rules::source::RegisterRole::Previous => state.previous.get(depth - 1),
        rules::source::RegisterRole::From => state.from.get(depth - 1),
    }
}

/// Lowers a quoted or otherwise embedded script under a caller-selected schema.
/// Ranges are local to `syntax`; callers that embed the fragment can map them
/// back to their containing token with the parser source map.
#[must_use]
pub fn lower_ir_schema(
    syntax: Arc<ParsedFile>,
    ir: &rules::ir::RulesIr,
    schema: rules::ir::SchemaId,
    subtypes: rules::ir::SubtypeSet,
    state: ScopeState,
    facts: &dyn rules::ir::SymbolFacts,
) -> HirFile {
    lower_ir_schema_range_impl(syntax, ir, schema, subtypes, state, facts, None, &[])
}

/// Lowers a trial instance with explicit virtual-hole byte ranges.
#[allow(clippy::too_many_arguments)]
pub fn lower_ir_schema_with_holes(
    syntax: Arc<ParsedFile>,
    ir: &rules::ir::RulesIr,
    schema: rules::ir::SchemaId,
    subtypes: rules::ir::SubtypeSet,
    state: ScopeState,
    facts: &dyn rules::ir::SymbolFacts,
    holes: &[text::TextRange],
) -> HirFile {
    lower_ir_schema_range_impl(syntax, ir, schema, subtypes, state, facts, None, holes)
}

/// Lowers the original CST inside a selected container without a standalone reparse.
#[allow(clippy::too_many_arguments)]
pub fn lower_ir_schema_in_range(
    syntax: Arc<ParsedFile>,
    ir: &rules::ir::RulesIr,
    schema: rules::ir::SchemaId,
    subtypes: rules::ir::SubtypeSet,
    state: ScopeState,
    facts: &dyn rules::ir::SymbolFacts,
    range: text::TextRange,
) -> HirFile {
    lower_ir_schema_range_impl(syntax, ir, schema, subtypes, state, facts, Some(range), &[])
}
#[allow(clippy::too_many_arguments)]
fn lower_ir_schema_range_impl(
    syntax: Arc<ParsedFile>,
    ir: &rules::ir::RulesIr,
    schema: rules::ir::SchemaId,
    subtypes: rules::ir::SubtypeSet,
    state: ScopeState,
    facts: &dyn rules::ir::SymbolFacts,
    range: Option<text::TextRange>,
    holes: &[text::TextRange],
) -> HirFile {
    let collected = collector::collect(&syntax);
    let ir_facts = ir_lowering::lower_schema_fragment(
        ir, &syntax, schema, subtypes, state, facts, range, holes,
    );
    let template_definitions = ir_facts
        .definitions
        .iter()
        .filter(|definition| {
            ir_facts
                .template_kinds
                .contains(&definition.kind.to_ascii_lowercase())
        })
        .cloned()
        .collect::<Vec<_>>();
    let (parameter_definitions, parameter_references) = parameters::lower_parameters(
        &syntax,
        &collected.properties,
        &collected.parameter_conditionals,
        None,
        &RuleSet::empty(),
        None,
        Some(&template_definitions),
    );
    let dynamic_templates = template_lowering::lower_dynamic_templates_ir(
        &syntax,
        &ir_facts.definitions,
        &collected.parameter_conditionals,
        &parameter_references,
        &ir_facts.template_kinds,
    );
    let mut analysis_coverage = ir_facts.analysis_coverage.clone();
    if template_definitions.iter().any(|definition| {
        !dynamic_templates
            .iter()
            .any(|template| template.definition_range == definition.range)
    }) {
        analysis_coverage
            .limits
            .insert(analysis::AnalysisLimit::UnavailableTemplate);
    }
    HirFile {
        analysis_coverage,
        overload_facts: ir_facts.overload_facts,
        syntax,
        scope: Scope::Unknown,
        properties: collected.properties,
        localisation_entries: collected.localisation_entries,
        bare_values: collected.bare_values,
        definitions: ir_facts.definitions,
        references: ir_facts.references,
        scope_facts: ir_facts.scope_facts,
        schema_facts: ir_facts.schema_facts,
        field_facts: ir_facts.field_facts,
        unknown_constructs: collected.unknown_constructs,
        parameter_conditionals: collected.parameter_conditionals,
        parameter_definitions,
        parameter_references,
        dynamic_templates,
        definition_attributes: ir_facts.definition_attributes,
        uses_ir: true,
        symbol_facts_dependency: ir_facts.symbol_facts_dependency.get(),
        binding_references: ir_facts.binding_references,
        runtime_parameter_guards: ir_facts.runtime_parameter_guards,
    }
}

fn lower_shared_impl(
    syntax: Arc<ParsedFile>,
    logical_path: Option<&LogicalPath>,
    _rules: &RuleSet,
    profile: Option<&GameProfile>,
    ir: Option<&rules::ir::RulesIr>,
    symbol_facts: Option<&dyn rules::ir::SymbolFacts>,
    retain_validation_facts: bool,
) -> HirFile {
    let collected = collector::collect(&syntax);
    let empty = rules::ir::RulesIr::empty();
    let selected = ir.unwrap_or(&empty);
    let anonymous = LogicalPath::parse("anonymous.txt").expect("logical path");
    let mut facts = ir_lowering::lower(
        selected,
        logical_path.unwrap_or(&anonymous),
        &collected.properties,
        &collected.bare_values,
        &syntax,
        symbol_facts,
        retain_validation_facts,
    );
    facts.definitions.extend(
        collected
            .localisation_entries
            .iter()
            .map(|entry| HirDefinition {
                kind: "localisation".into(),
                name: entry.name.clone(),
                range: entry.range,
                selection_range: entry.name_range,
            }),
    );
    let mut seen = facts
        .references
        .drain(..)
        .map(|reference| (reference.kind.to_ascii_lowercase(), reference))
        .collect::<Vec<_>>();
    seen.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
            .then_with(|| left.1.range.cmp(&right.1.range))
    });
    seen.dedup_by(|left, right| {
        left.0 == right.0 && left.1.name == right.1.name && left.1.range == right.1.range
    });
    let references = seen.into_iter().map(|(_, reference)| reference).collect();
    let template_definitions = facts
        .definitions
        .iter()
        .filter(|definition| {
            facts
                .template_kinds
                .contains(&definition.kind.to_ascii_lowercase())
        })
        .cloned()
        .collect::<Vec<_>>();
    let (parameter_definitions, parameter_references) = parameters::lower_parameters(
        &syntax,
        &collected.properties,
        &collected.parameter_conditionals,
        logical_path,
        &RuleSet::empty(),
        profile,
        Some(&template_definitions),
    );
    let dynamic_templates = template_lowering::lower_dynamic_templates_ir(
        &syntax,
        &facts.definitions,
        &collected.parameter_conditionals,
        &parameter_references,
        &facts.template_kinds,
    );
    let mut analysis_coverage = facts.analysis_coverage.clone();
    if template_definitions.iter().any(|definition| {
        !dynamic_templates
            .iter()
            .any(|template| template.definition_range == definition.range)
    }) {
        analysis_coverage
            .limits
            .insert(analysis::AnalysisLimit::UnavailableTemplate);
    }
    HirFile {
        analysis_coverage,
        overload_facts: facts.overload_facts,
        syntax,
        scope: Scope::Unknown,
        properties: collected.properties,
        localisation_entries: collected.localisation_entries,
        bare_values: collected.bare_values,
        definitions: facts.definitions,
        references,
        scope_facts: facts.scope_facts,
        unknown_constructs: collected.unknown_constructs,
        parameter_conditionals: collected.parameter_conditionals,
        parameter_definitions,
        parameter_references,
        dynamic_templates,
        definition_attributes: facts.definition_attributes,
        uses_ir: ir.is_some_and(|ir| !ir.schemas.is_empty()),
        symbol_facts_dependency: facts.symbol_facts_dependency.get(),
        schema_facts: facts.schema_facts,
        field_facts: facts.field_facts,
        runtime_parameter_guards: facts.runtime_parameter_guards,
        binding_references: facts.binding_references,
    }
}

/// Required and optional declared localisation mappings for hover.
#[must_use]
pub fn derived_localisation_references_for_hover(
    hir: &HirFile,
    _logical_path: &LogicalPath,
    _rules: &RuleSet,
) -> Vec<HirReference> {
    hir.binding_references_for_hover()
        .iter()
        .filter(|r| r.kind.eq_ignore_ascii_case("localisation"))
        .cloned()
        .collect()
}

/// Required and optional declared sprite mappings for hover.
#[must_use]
pub fn derived_sprite_references_for_hover(
    hir: &HirFile,
    _logical_path: &LogicalPath,
    _rules: &RuleSet,
) -> Vec<HirReference> {
    hir.binding_references_for_hover()
        .iter()
        .filter(|r| r.kind.eq_ignore_ascii_case("sprite"))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;

/// Case-insensitive ASCII prefix test that allocates nothing.
pub fn ascii_ci_starts_with(haystack: &str, needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack.is_char_boundary(needle.len())
        && haystack.as_bytes()[..needle.len()]
            .iter()
            .zip(needle.as_bytes())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}
