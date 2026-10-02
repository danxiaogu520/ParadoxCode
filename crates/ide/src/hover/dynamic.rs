//! Dynamic-definition hovers: call-site parameters and callable signatures.

use super::render::{HoverModel, code_span};
use crate::support::{ParsedInput, contains};
use crate::types::{CancellationToken, Cancelled};
use engine::AnalysisSnapshot;
use text::TextSize;

/// Callable argument keys are tied to the IR reference selected at the call.
pub(crate) fn ir_invocation_parameter_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let Some(argument) = hir
        .properties()
        .iter()
        .find(|property| contains(property.key_range, position))
    else {
        return Ok(None);
    };
    let Some(invocation) = hir
        .properties()
        .iter()
        .filter(|property| {
            property.range.start() < argument.range.start()
                && property.range.end() >= argument.range.end()
                && property.path.len() + 1 == argument.path.len()
        })
        .min_by_key(|property| property.range.len())
    else {
        return Ok(None);
    };
    let Some(reference) = hir
        .references()
        .iter()
        .find(|reference| reference.range == invocation.key_range)
    else {
        return Ok(None);
    };
    let Some(summary) =
        crate::semantic::dynamic_definition_summary(snapshot, &reference.kind, &reference.name)
    else {
        return Ok(None);
    };
    let Some(parameter) = summary
        .parameters
        .iter()
        .find(|parameter| parameter.name.eq_ignore_ascii_case(&argument.key))
    else {
        return Ok(None);
    };
    let mut model = HoverModel::new(format!(
        "### parameter {} of scripted {}",
        code_span(&parameter.name),
        code_span(&summary.name)
    ));
    let mut section = format!(
        "- Presence: `{}`",
        if crate::dynamic_rules::parameter_effectively_required(snapshot, &summary, parameter) {
            "required"
        } else {
            "optional"
        }
    );
    let sites = crate::ir_callable::parameter_sites(
        snapshot,
        &reference.kind,
        &reference.name,
        &parameter.name,
        &crate::ir_callable::invocation_bindings(hir, invocation),
        crate::ir_callable::invocation_state(hir, invocation),
        cancellation,
    )?;
    if let Some(lines) = ir_parameter_contract_lines(snapshot, &summary, &parameter.name, &sites) {
        section.push('\n');
        section.push_str(&lines);
    }
    model.push_section(section);
    Ok(Some(model))
}

/// Shared contract lines for a dynamic parameter at its definition site:
/// resolves the row by kind (or name alone when the caller does not know the
/// kind) and replays the parameter's usage-site rows under an any-scope,
/// showing every conditional branch.
pub(crate) fn dynamic_parameter_contract_lines(
    snapshot: &AnalysisSnapshot,
    owner_kind: Option<&str>,
    owner_name: &str,
    parameter_name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
    cancellation.checkpoint()?;

    let summary = if let Some(kind) = owner_kind {
        crate::semantic::dynamic_definition_summary(snapshot, kind, owner_name)
    } else {
        snapshot.ir().types.iter().find_map(|info| {
            let kind = snapshot.ir().strings().resolve(info.name);
            crate::semantic::dynamic_definition_type(snapshot, kind)
                .then(|| crate::semantic::dynamic_definition_summary(snapshot, kind, owner_name))
                .flatten()
        })
    };
    let Some(summary) = summary else {
        return Ok(None);
    };
    let sites = crate::ir_callable::definition_parameter_sites(
        snapshot,
        &summary.kind,
        owner_name,
        parameter_name,
        cancellation,
    )?;
    Ok(ir_parameter_contract_lines(
        snapshot,
        &summary,
        parameter_name,
        &sites,
    ))
}

fn ir_parameter_contract_lines(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
    parameter: &str,
    sites: &[crate::ir_callable::ParameterSite],
) -> Option<String> {
    use crate::ir_callable::Domain;
    summary
        .parameters
        .iter()
        .find(|item| item.name.eq_ignore_ascii_case(parameter))?;
    let mut lines = Vec::new();
    if sites
        .iter()
        .any(|site| matches!(site.domain, Domain::Payload { .. }))
    {
        lines.push(
            "- Payload: quoted script (the caller's raw text is spliced into the body)".into(),
        );
    }
    if sites
        .iter()
        .any(|site| matches!(site.domain, Domain::Key { .. }))
    {
        lines.push("- Dispatch: rendered as a statement key in the body".into());
    }
    let mut values = Vec::new();
    for site in sites {
        match &site.domain {
            Domain::Value(matchers) => {
                let expected = matchers
                    .iter()
                    .map(|id| ir_parameter_value_label(snapshot.ir(), *id))
                    .collect::<Vec<_>>()
                    .join(" or ");
                let rendered = site
                    .rendered_value(parameter, "…")
                    .unwrap_or_else(|| "…".into());
                if rendered == "…" {
                    values.push(expected);
                } else {
                    lines.push(format!("- Renders as `{rendered}` at its usage site, where the value must be {expected}"));
                }
            }
            Domain::Key { .. } => {
                if let Some(rendered) = site.rendered_value(parameter, "…")
                    && rendered != "…"
                {
                    lines.push(format!(
                        "- Renders as statement key `{rendered}` in the body"
                    ));
                }
            }
            Domain::Payload { .. } | Domain::Unresolved => {}
        }
    }
    if !values.is_empty() {
        lines.push(format!(
            "- Inferred value constraints (per usage site): {}",
            values.join("; ")
        ));
    }
    if lines.is_empty() {
        lines.push("- Inferred value constraints: none".into());
    }
    Some(lines.join("\n"))
}

fn ir_parameter_value_label(ir: &rules::ir::RulesIr, id: rules::ir::MatcherId) -> String {
    match ir.matcher(id) {
        rules::ir::Matcher::Ref(rules::ir::RefTarget::Type { type_id, .. }) => {
            let info = ir.type_info(*type_id);
            let examples = info
                .builtin
                .iter()
                .take(8)
                .map(|value| code_span(ir.strings().resolve(*value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "symbol type `{}`{}",
                ir.strings().resolve(info.name),
                if examples.is_empty() {
                    String::new()
                } else {
                    format!(" (including {examples})")
                }
            )
        }
        rules::ir::Matcher::Union(items) => items
            .iter()
            .map(|id| ir_parameter_value_label(ir, *id))
            .collect::<Vec<_>>()
            .join(" or "),
        _ => crate::ir_semantic::describe(ir, id),
    }
}

/// Renders the `#### Callable signature` section for a dynamic definition symbol hover.
///
/// The invocation form matches the completion snippet's model: scalar only
/// for parameterless definitions, otherwise a named parameter block. Required
/// and optional group by activation scoping — the same partition that picks
/// snippet tabstops — so hover, completion, and diagnostics agree on which
/// parameters an invocation may omit.
pub(crate) fn dynamic_signature_hover(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
) -> String {
    let invocation = match summary.parameters.len() {
        0 => format!("`{} = yes`", summary.name),
        _ => "named parameter block".to_owned(),
    };
    let required_presence = |parameter: &engine::DynamicParameterSignature| {
        crate::dynamic_rules::parameter_effectively_required(snapshot, summary, parameter)
    };
    let required = summary
        .parameters
        .iter()
        .filter(|parameter| required_presence(parameter))
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    let optional = summary
        .parameters
        .iter()
        .filter(|parameter| !required_presence(parameter))
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    let mut lines = vec![format!("- Invocation: {invocation}")];
    if !required.is_empty() {
        lines.push(format!("- Required parameters: {}", required.join(", ")));
    }
    if !optional.is_empty() {
        lines.push(format!("- Optional parameters: {}", optional.join(", ")));
    }
    if summary.parameters.is_empty() {
        lines.push("- Parameters: none".to_owned());
    }
    format!("#### Callable signature\n\n{}", lines.join("\n"))
}

/// Presence of one parameter of a resolved dynamic definition under the
/// invocation-form model. Returns `None` when the definition or its template
/// cannot be resolved, leaving callers to fall back to the indexed `required`
/// flag.
pub(crate) fn parameter_presence_required(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
) -> Option<bool> {
    let template = crate::semantic::resolve_dynamic_definition(snapshot, kind, name)?
        .summary
        .template;
    let template = template.as_ref()?;
    Some(!crate::dynamic_rules::parameter_is_activation_scoped(
        snapshot, template, parameter,
    ))
}
