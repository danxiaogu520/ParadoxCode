//! Dynamic-definition hovers: call-site parameters and callable signatures.

use super::render::{HoverModel, code_span};
use super::rules::semantic_value_hover_label;
use crate::completion::{dynamic_parameter_owner, semantic_completion_context_with_cancellation};
use crate::support::{ParsedInput, contains};
use crate::types::{CancellationToken, Cancelled};
use pdx_engine::AnalysisSnapshot;
use pdx_text::TextSize;

/// Hover for an argument key at a dynamic call site (`stable = { AMT = 1 }`):
/// resolves the definition's parameter row so the hover shows the parameter
/// contract instead of the generic property rows behind the parameter enum.
pub(crate) fn dynamic_invocation_parameter_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    let Some(context) =
        semantic_completion_context_with_cancellation(snapshot, input, position, cancellation)?
    else {
        return Ok(None);
    };
    let Some(property) = context.property.as_ref() else {
        return Ok(None);
    };
    if !contains(property.key_range, position) {
        return Ok(None);
    }
    let Some(invocation) = context.container_property.as_ref() else {
        return Ok(None);
    };
    let Some((owner_kind, owner_name, _)) =
        dynamic_parameter_owner(snapshot, &context, property, invocation)
    else {
        return Ok(None);
    };
    let Some(row) = crate::dynamic_rules::dynamic_rule_row(snapshot, &owner_kind, &owner_name)
    else {
        return Ok(None);
    };
    let Some(parameter) = row
        .parameters
        .iter()
        .find(|parameter| parameter.name.eq_ignore_ascii_case(&property.key))
    else {
        return Ok(None);
    };
    let mut model = HoverModel::new(format!(
        "### parameter {} of scripted {}",
        code_span(&parameter.name),
        code_span(&row.name),
    ));
    let mut section = format!(
        "- Presence: `{}`",
        if parameter.required {
            "required"
        } else {
            "optional"
        },
    );
    if let Some(contract) =
        dynamic_parameter_contract_lines(snapshot, Some(&owner_kind), &owner_name, &property.key)
    {
        section.push('\n');
        section.push_str(&contract);
    }
    model.push_section(section);
    Ok(Some(model))
}

/// Shared contract lines for a dynamic parameter: payload nature, dispatch,
/// forwarding edges, and the value constraints its usage sites imply
/// (following forwarding edges into nested calls). Constraint labels are
/// self-contained code spans and must not be wrapped again.
pub(crate) fn dynamic_parameter_contract_lines(
    snapshot: &AnalysisSnapshot,
    owner_kind: Option<&str>,
    owner_name: &str,
    parameter_name: &str,
) -> Option<String> {
    let row = owner_kind
        .and_then(|kind| crate::dynamic_rules::dynamic_rule_row(snapshot, kind, owner_name))
        .or_else(|| crate::dynamic_rules::dynamic_rule_row_by_name(snapshot, owner_name))?;
    let parameter = row
        .parameters
        .iter()
        .find(|parameter| parameter.name.eq_ignore_ascii_case(parameter_name))?;
    let mut lines = Vec::new();
    if parameter.quoted_script {
        lines.push(
            "- Payload: quoted script (the caller's raw text is spliced into the body)".to_owned(),
        );
    }
    if parameter.used_in_key {
        lines.push("- Dispatch: rendered as a statement key in the body".to_owned());
    }
    for edge in &parameter.forwarded_to {
        let target = match &edge.parameter {
            Some(name) => format!("`{}` (parameter `{}`)", edge.name, name),
            None => format!("`{}` (rendered parameter key)", edge.name),
        };
        lines.push(format!("- Forwarded to scripted {target}"));
    }
    let sites =
        crate::diagnostics::effective_parameter_sites(snapshot, &row, parameter, &mut Vec::new());
    if !sites.is_empty() {
        let rendered = sites
            .iter()
            .map(|matchers| {
                matchers
                    .iter()
                    .map(semantic_value_hover_label)
                    .collect::<Vec<_>>()
                    .join(" or ")
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!(
            "- Inferred value constraints (per usage site): {rendered}"
        ));
    }
    for site in &parameter.affixed_sites {
        let expected = site
            .matchers
            .iter()
            .map(semantic_value_hover_label)
            .collect::<Vec<_>>()
            .join(" or ");
        lines.push(format!(
            "- Renders as `{}…{}` at its usage site, where the value must be {expected}",
            site.prefix, site.suffix
        ));
    }
    if sites.is_empty()
        && parameter.affixed_sites.is_empty()
        && !parameter.quoted_script
        && !parameter.used_in_key
        && parameter.forwarded_to.is_empty()
    {
        lines.push("- Inferred value constraints: none".to_owned());
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Renders the `#### Callable signature` section for a dynamic definition symbol hover.
pub(crate) fn dynamic_signature_hover(summary: &pdx_engine::DynamicDefinitionSummary) -> String {
    let invocation = match summary.parameters.len() {
        0 => format!("`{} = yes`", summary.name),
        _ => "named parameter block".to_owned(),
    };
    let required = summary
        .parameters
        .iter()
        .filter(|parameter| parameter.required)
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    let optional = summary
        .parameters
        .iter()
        .filter(|parameter| !parameter.required)
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
