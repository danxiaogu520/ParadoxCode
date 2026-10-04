//! Presence projections from the shared Template program, used by hover and snippets.
use engine::{AnalysisSnapshot, DynamicParameterSignature};

/// Whether a parameter has no unguarded text read in the shared definition program.
pub(crate) fn parameter_is_activation_scoped(
    _snapshot: &AnalysisSnapshot,
    template: &hir::Template,
    name: &str,
) -> bool {
    !template
        .program
        .unconditional_reads()
        .contains(&name.to_ascii_lowercase())
}

/// Definition-side presence summary. Call-side activation is handled by the shared interpreter.
pub(crate) fn parameter_effectively_required(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
    parameter: &DynamicParameterSignature,
) -> bool {
    summary
        .template
        .as_ref()
        .map_or(parameter.required, |template| {
            !parameter_is_activation_scoped(snapshot, template, &parameter.name)
        })
}
