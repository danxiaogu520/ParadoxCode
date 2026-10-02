//! Callable parameter presence from declared templates.
use crate::semantic::resolve_dynamic_definition;
use engine::{AnalysisSnapshot, DynamicParameterSignature};
use hir::{TemplateFragment, TemplateItem, TemplateProperty, TemplateToken, TemplateValue};
/// Whether every use of `name` in the definition body is activation-scoped:
/// inside a `[[conditional]]` chunk, or forwarded as an argument into a nested
/// dynamic invocation of the same kind. The game drops inactive conditional
/// chunks and delegates forwarded arguments to the callee, so activation-scoped
/// parameters are the only ones an invocation may omit. Every other parameter
/// is effectively required: the engine substitutes the body textually, and an
/// unbound `$PARAM$` outside inactive chunks produces broken script even when
/// the use sits in a runtime `if`/`else` branch.
pub(crate) fn parameter_is_activation_scoped(
    snapshot: &AnalysisSnapshot,
    template: &hir::Template,
    name: &str,
) -> bool {
    let mut seen = false;
    let mut seen_unscoped = false;
    activation_scope_items(
        snapshot,
        template,
        &template.items,
        false,
        name,
        &mut seen,
        &mut seen_unscoped,
    );
    seen && !seen_unscoped
}
/// Visits template items tracking whether parameter uses sit inside
/// conditional chunks or forwarded argument blocks. `scoped` marks the
/// activation-scoped state inherited from enclosing containers.
fn activation_scope_items(
    snapshot: &AnalysisSnapshot,
    template: &hir::Template,
    items: &[TemplateItem],
    scoped: bool,
    name: &str,
    seen: &mut bool,
    seen_unscoped: &mut bool,
) {
    for item in items {
        match item {
            TemplateItem::Property(property) => {
                activation_scope_token(&property.key, scoped, name, seen, seen_unscoped);
                match &property.value {
                    TemplateValue::Scalar(token) => {
                        activation_scope_token(token, scoped, name, seen, seen_unscoped);
                    }
                    TemplateValue::Block { items, .. } => {
                        // A block whose key names another dynamic definition
                        // forwards its arguments; the callee's own signature
                        // decides whether the parameter matters there.
                        let child_scoped =
                            scoped || activation_forwards_arguments(snapshot, template, property);
                        activation_scope_items(
                            snapshot,
                            template,
                            items,
                            child_scoped,
                            name,
                            seen,
                            seen_unscoped,
                        );
                    }
                }
            }
            TemplateItem::BareValue(token) => {
                activation_scope_token(token, scoped, name, seen, seen_unscoped);
            }
            TemplateItem::Conditional(conditional) => {
                // The guard parameter's only appearance is the `[[name]`
                // marker itself; it is activation-scoped by construction.
                if conditional.name.eq_ignore_ascii_case(name) {
                    *seen = true;
                }
                activation_scope_items(
                    snapshot,
                    template,
                    &conditional.items,
                    true,
                    name,
                    seen,
                    seen_unscoped,
                );
            }
        }
    }
}
fn activation_scope_token(
    token: &TemplateToken,
    scoped: bool,
    name: &str,
    seen: &mut bool,
    seen_unscoped: &mut bool,
) {
    for fragment in &token.fragments {
        if let TemplateFragment::Parameter {
            name: parameter, ..
        } = fragment
            && parameter.eq_ignore_ascii_case(name)
        {
            *seen = true;
            *seen_unscoped |= !scoped;
        }
    }
}
fn activation_forwards_arguments(
    snapshot: &AnalysisSnapshot,
    template: &hir::Template,
    property: &TemplateProperty,
) -> bool {
    let [TemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    resolve_dynamic_definition(snapshot, &template.kind, key.trim()).is_some()
}
/// Effective presence of a signature parameter for the invocation form model:
/// scalar form exists only for parameterless definitions, otherwise the block
/// form carries one tabstop per effectively-required parameter. Template-backed
/// summaries use activation scoping as the single authority (runtime branches
/// and same-named forwarding do not relax it); summaries without a template
/// fall back to the indexed `required` flag.
pub(crate) fn parameter_effectively_required(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
    parameter: &DynamicParameterSignature,
) -> bool {
    match summary.template.as_ref() {
        Some(template) => !parameter_is_activation_scoped(snapshot, template, &parameter.name),
        None => parameter.required,
    }
}
