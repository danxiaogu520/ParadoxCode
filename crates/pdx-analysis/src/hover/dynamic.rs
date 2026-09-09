//! Dynamic-definition hovers: call-site parameters and callable signatures.

use super::render::{HoverModel, code_span};
use super::rules::semantic_value_hover_label;
use crate::completion::{
    HoverCaller, dynamic_parameter_owner, replay_parameter_sites_for_hover,
    semantic_completion_context_with_cancellation,
};
use crate::dynamic_rules::{
    DynamicParameterRow, DynamicRuleRow, dynamic_rule_row, dynamic_rule_row_by_name,
};
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
    let Some((owner_kind, owner_name, caller_scope)) =
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
    if let Some(contract) = parameter_contract_lines(
        snapshot,
        &row,
        parameter,
        Some(&HoverCaller {
            invocation,
            target: property,
            scope: caller_scope,
        }),
    ) {
        section.push('\n');
        section.push_str(&contract);
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
) -> Option<String> {
    let row = owner_kind
        .and_then(|kind| dynamic_rule_row(snapshot, kind, owner_name))
        .or_else(|| dynamic_rule_row_by_name(snapshot, owner_name))?;
    let parameter = row
        .parameters
        .iter()
        .find(|parameter| parameter.name.eq_ignore_ascii_case(parameter_name))?;
    parameter_contract_lines(snapshot, &row, parameter, None)
}

/// Contract lines for one dynamic parameter, rendered from the same replayed
/// site rows completion consumes: payload nature, dispatch, forwarding
/// edges, and the value constraints the definition's usage sites imply.
/// Constraint labels are self-contained code spans and must not be wrapped
/// again.
fn parameter_contract_lines(
    snapshot: &AnalysisSnapshot,
    row: &DynamicRuleRow,
    parameter: &DynamicParameterRow,
    caller: Option<&HoverCaller<'_>>,
) -> Option<String> {
    let replayed = replay_parameter_sites_for_hover(snapshot, row, parameter, caller);
    let mut lines = Vec::new();
    if parameter.quoted_script {
        lines.push(
            "- Payload: quoted script (the caller's raw text is spliced into the body)".to_owned(),
        );
    }
    if parameter.used_in_key {
        lines.push("- Dispatch: rendered as a statement key in the body".to_owned());
    }
    for (prefix, suffix) in key_render_forms(&replayed) {
        lines.push(format!(
            "- Renders as statement key `{prefix}…{suffix}` in the body"
        ));
    }
    for edge in &parameter.forwarded_to {
        let target = match &edge.parameter {
            Some(name) => format!("`{}` (parameter `{}`)", edge.name, name),
            None => format!("`{}` (rendered parameter key)", edge.name),
        };
        lines.push(format!("- Forwarded to scripted {target}"));
    }
    if !replayed.values.is_empty() {
        let rendered = replayed
            .values
            .iter()
            .map(|site| {
                site.matchers
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
    for site in &replayed.affixed {
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
    if replayed.values.is_empty()
        && replayed.affixed.is_empty()
        && replayed.key_renders.is_empty()
        && replayed.quoted_scripts.is_empty()
        && parameter.forwarded_to.is_empty()
        && !parameter.quoted_script
        && !parameter.used_in_key
    {
        lines.push("- Inferred value constraints: none".to_owned());
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Distinct literal affix pairs of replayed key-render sites (`set_$KIND$_policy`
/// yields `set_…_policy`); wildcard renders (both affixes empty) are covered by
/// the generic dispatch line.
fn key_render_forms(replayed: &crate::completion::ReplayedSites) -> Vec<(String, String)> {
    let mut forms = Vec::new();
    for site in &replayed.key_renders {
        if site.prefix.is_empty() && site.suffix.is_empty() {
            continue;
        }
        if !forms.iter().any(|(prefix, suffix): &(String, String)| {
            prefix.eq_ignore_ascii_case(&site.prefix) && suffix.eq_ignore_ascii_case(&site.suffix)
        }) {
            forms.push((site.prefix.clone(), site.suffix.clone()));
        }
    }
    forms
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
