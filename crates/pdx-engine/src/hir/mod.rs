//! Rule-aware, game-independent semantic lowering boundary.

use std::sync::Arc;

use pdx_parser::ParsedFile;
use pdx_rules::{GameProfile, RuleSet};
use pdx_text::LogicalPath;

mod collector;
mod model;
mod parameters;
mod scope;
mod semantics;
mod templates;

pub use model::*;
pub use semantics::{
    semantic_file_root_context, semantic_root_context, semantic_root_context_is_fallback,
    semantic_type_path_matches,
};

#[cfg(test)]
pub(crate) use scope::{
    StaticTransitionInput, child_key_may_match, child_scope_state, property_children,
    repeated_scope_register_depth, resolve_scope_expression, statically_selected_transition,
};

fn range_within(inner: pdx_text::TextRange, outer: pdx_text::TextRange) -> bool {
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
    lower_shared_impl(syntax, None, rules, None)
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
    lower_shared_impl(syntax, Some(logical_path), rules, Some(profile))
}

fn lower_shared_impl(
    syntax: Arc<ParsedFile>,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> HirFile {
    let collected = collector::collect(&syntax);
    let properties = collected.properties;
    let localisation_entries = collected.localisation_entries;
    let bare_values = collected.bare_values;
    let unknown_constructs = collected.unknown_constructs;
    let parameter_conditionals = collected.parameter_conditionals;
    let scope_facts = scope::lower_scope_facts(&properties, logical_path, rules, profile);
    let (definitions, mut references) = semantics::lower_semantics(
        &properties,
        &localisation_entries,
        &bare_values,
        logical_path,
        rules,
        profile,
        &scope_facts,
    );
    references.extend(semantics::derived_localisation_references(
        &properties,
        syntax.root().range(),
        logical_path,
        rules,
        false,
    ));
    let mut seen_references = std::collections::BTreeSet::new();
    references.retain(|reference| {
        seen_references.insert((
            reference.kind.to_ascii_lowercase(),
            reference.name.clone(),
            reference.range,
        ))
    });
    let (parameter_definitions, parameter_references) = parameters::lower_parameters(
        &syntax,
        &properties,
        &parameter_conditionals,
        logical_path,
        rules,
        profile,
    );
    let macro_templates = templates::lower_macro_templates(
        &syntax,
        &definitions,
        &parameter_conditionals,
        &parameter_references,
        rules,
    );
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
        macro_templates,
    }
}

/// Returns all type-instance localisation mappings for hover/navigation queries.
///
/// Required mappings are part of the normal HIR reference set because they also drive missing
/// localisation diagnostics. Optional mappings are intentionally kept out of that set: a missing
/// optional key is valid game data. Hover can still ask for the complete mapping set and resolve
/// only keys that actually exist in the workspace.
#[must_use]
pub fn derived_localisation_references_for_hover(
    hir: &HirFile,
    logical_path: &LogicalPath,
    rules: &RuleSet,
) -> Vec<HirReference> {
    semantics::derived_localisation_references(
        &hir.properties,
        hir.syntax.root().range(),
        Some(logical_path),
        rules,
        true,
    )
}

#[cfg(test)]
mod tests;
