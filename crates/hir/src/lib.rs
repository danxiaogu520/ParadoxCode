//! Rule-aware, game-independent semantic lowering boundary.

use std::sync::Arc;

use parser::ParsedFile;
use rules::{GameProfile, RuleSet};
use text::LogicalPath;

mod collector;
mod ir_lowering;
mod model;
mod parameters;
mod scope;
mod semantics;
mod templates;

pub use model::*;
pub use semantics::ascii_ci_starts_with;
pub use semantics::{
    semantic_file_root_context, semantic_root_context, semantic_root_context_is_fallback,
    semantic_type_path_matches,
};

#[cfg(test)]
pub(crate) use scope::{
    StaticTransitionInput, child_key_may_match, child_scope_state, property_children,
    repeated_scope_register_depth, resolve_scope_expression, statically_selected_transition,
};

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
    lower_shared_impl(syntax, None, rules, None, None, None)
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
    lower_shared_impl(syntax, Some(logical_path), rules, Some(profile), None, None)
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
    )
}

/// Lowers a quoted or otherwise embedded script under a caller-selected schema.
/// Ranges are local to `syntax`; callers that embed the fragment can map them
/// back to their containing token with the parser source map.
#[must_use]
pub fn lower_ir_schema<F: rules::ir::SymbolFacts>(
    syntax: Arc<ParsedFile>,
    ir: &rules::ir::RulesIr,
    schema: rules::ir::SchemaId,
    subtypes: rules::ir::SubtypeSet,
    state: ScopeState,
    facts: &F,
) -> HirFile {
    let collected = collector::collect(&syntax);
    let ir_facts = ir_lowering::lower_schema_fragment(ir, &syntax, schema, subtypes, state, facts);
    let callable_definitions = ir_facts
        .definitions
        .iter()
        .filter(|definition| {
            ir_facts
                .callable_kinds
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
        Some(&callable_definitions),
    );
    let dynamic_templates = templates::lower_dynamic_templates_ir(
        &syntax,
        &ir_facts.definitions,
        &collected.parameter_conditionals,
        &parameter_references,
        &ir_facts.callable_kinds,
    );
    HirFile {
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
        binding_references: ir_facts.binding_references,
        runtime_parameter_guards: ir_facts.runtime_parameter_guards,
    }
}

fn lower_shared_impl(
    syntax: Arc<ParsedFile>,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
    ir: Option<&rules::ir::RulesIr>,
    symbol_facts: Option<&dyn rules::ir::SymbolFacts>,
) -> HirFile {
    let ir = ir.filter(|ir| !ir.files.is_empty() || !ir.schemas.is_empty());
    let collected = collector::collect(&syntax);
    let properties = collected.properties;
    let localisation_entries = collected.localisation_entries;
    let bare_values = collected.bare_values;
    let unknown_constructs = collected.unknown_constructs;
    let parameter_conditionals = collected.parameter_conditionals;
    let ir_facts = ir.map(|ir| {
        ir_lowering::lower(
            ir,
            logical_path.expect("IR lowering requires a logical path"),
            &properties,
            &bare_values,
            &syntax,
            symbol_facts,
        )
    });
    let scope_facts = ir_facts.as_ref().map_or_else(
        || scope::lower_scope_facts(&properties, logical_path, rules, profile),
        |facts| facts.scope_facts.clone(),
    );
    let (mut definitions, mut references, definition_attributes) = if let Some(facts) = &ir_facts {
        (
            facts.definitions.clone(),
            facts.references.clone(),
            facts.definition_attributes.clone(),
        )
    } else {
        semantics::lower_semantics(
            &properties,
            &localisation_entries,
            &bare_values,
            logical_path,
            rules,
            profile,
            &scope_facts,
        )
    };
    if ir.is_some() {
        definitions.extend(localisation_entries.iter().map(|entry| HirDefinition {
            kind: "localisation".into(),
            name: entry.name.clone(),
            range: entry.range,
            selection_range: entry.name_range,
        }));
    }
    if ir_facts.is_none() {
        // A derived entry whose name the semantic layer already types with the
        // same kind (a schema-typed `icon = <sprite>` value, a `title = <key>`
        // field) would duplicate that rule's diagnostic; the derived reference
        // adds nothing there, so keep only names the semantic layer missed.
        let semantically_typed = references
            .iter()
            .filter(|reference| {
                matches!(
                    reference.origin,
                    HirReferenceOrigin::SemanticTyped | HirReferenceOrigin::Semantic
                ) && (reference.kind.eq_ignore_ascii_case("sprite")
                    || reference.kind.eq_ignore_ascii_case("localisation"))
            })
            .map(|reference| {
                (
                    reference.kind.to_ascii_lowercase(),
                    reference.name.to_ascii_lowercase(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut derived = semantics::derived_localisation_references(
            &properties,
            syntax.root().range(),
            logical_path,
            rules,
            true,
        );
        derived.extend(semantics::derived_sprite_references(
            &properties,
            syntax.root().range(),
            logical_path,
            rules,
            true,
        ));
        references.extend(derived.into_iter().filter(|reference| {
            !semantically_typed.contains(&(
                reference.kind.to_ascii_lowercase(),
                reference.name.to_ascii_lowercase(),
            ))
        }));
    }
    // Deduplicate (case-insensitive kind, name, range) without a BTreeSet:
    // sorting the moved tuples once avoids cloning every reference name and
    // lowercasing every kind on every insert.
    let mut seen_references = references
        .drain(..)
        .map(|reference| (reference.kind.to_ascii_lowercase(), reference))
        .collect::<Vec<_>>();
    seen_references.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
            .then_with(|| left.1.range.cmp(&right.1.range))
    });
    seen_references.dedup_by(|left, right| {
        left.0 == right.0 && left.1.name == right.1.name && left.1.range == right.1.range
    });
    let references = seen_references
        .into_iter()
        .map(|(_, reference)| reference)
        .collect::<Vec<_>>();
    let ir_dynamic_definitions = ir_facts.as_ref().map(|facts| {
        facts
            .definitions
            .iter()
            .filter(|definition| {
                facts
                    .callable_kinds
                    .contains(&definition.kind.to_ascii_lowercase())
            })
            .cloned()
            .collect::<Vec<_>>()
    });
    let (parameter_definitions, parameter_references) = parameters::lower_parameters(
        &syntax,
        &properties,
        &parameter_conditionals,
        logical_path,
        rules,
        profile,
        ir_dynamic_definitions.as_deref(),
    );
    let dynamic_templates = if let Some(facts) = &ir_facts {
        templates::lower_dynamic_templates_ir(
            &syntax,
            &definitions,
            &parameter_conditionals,
            &parameter_references,
            &facts.callable_kinds,
        )
    } else {
        templates::lower_dynamic_templates(
            &syntax,
            &definitions,
            &parameter_conditionals,
            &parameter_references,
            rules,
        )
    };
    HirFile {
        syntax,
        scope: Scope::Unknown,
        properties,
        localisation_entries,
        bare_values,
        definitions,
        references,
        scope_facts,
        unknown_constructs,
        parameter_conditionals,
        parameter_definitions,
        parameter_references,
        dynamic_templates,
        definition_attributes,
        uses_ir: ir.is_some(),
        schema_facts: ir_facts
            .as_ref()
            .map_or_else(Vec::new, |facts| facts.schema_facts.clone()),
        field_facts: ir_facts
            .as_ref()
            .map_or_else(Vec::new, |facts| facts.field_facts.clone()),
        runtime_parameter_guards: ir_facts
            .as_ref()
            .map_or_else(Vec::new, |facts| facts.runtime_parameter_guards.clone()),
        binding_references: ir_facts
            .as_ref()
            .map_or_else(Vec::new, |facts| facts.binding_references.clone()),
    }
}

/// Returns all type-instance localisation mappings for hover/navigation queries.
///
/// Required mappings are part of the normal HIR reference set because they also drive missing
/// localisation diagnostics. Non-required mappings are intentionally kept out of that set.
/// Hover asks for the complete mapping set and resolves only keys that actually exist in the
/// workspace.
#[must_use]
pub fn derived_localisation_references_for_hover(
    hir: &HirFile,
    logical_path: &LogicalPath,
    rules: &RuleSet,
) -> Vec<HirReference> {
    if hir.uses_ir() {
        return hir
            .binding_references_for_hover()
            .iter()
            .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
            .cloned()
            .collect();
    }
    semantics::derived_localisation_references(
        &hir.properties,
        hir.syntax.root().range(),
        Some(logical_path),
        rules,
        false,
    )
}

/// Returns all type-instance sprite mappings for hover queries.
///
/// Icon bindings ride the same reference set as localisation mappings; hover
/// resolves only names that exist as sprites in the workspace.
#[must_use]
pub fn derived_sprite_references_for_hover(
    hir: &HirFile,
    logical_path: &LogicalPath,
    rules: &RuleSet,
) -> Vec<HirReference> {
    if hir.uses_ir() {
        return hir
            .binding_references_for_hover()
            .iter()
            .filter(|reference| reference.kind.eq_ignore_ascii_case("sprite"))
            .cloned()
            .collect();
    }
    semantics::derived_sprite_references(
        &hir.properties,
        hir.syntax.root().range(),
        Some(logical_path),
        rules,
        false,
    )
}

#[cfg(test)]
mod tests;
