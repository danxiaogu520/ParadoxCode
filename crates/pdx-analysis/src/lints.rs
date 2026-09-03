//! Structural lints for EU4 logic containers.
//!
//! These checks are purely syntactic-structural: they fire on authored script
//! shapes that the game loads but whose written meaning differs from the
//! obvious reading (NOT with several conditions), that are dead or redundant
//! (constant conditions, empty blocks), or that break branch chaining (an
//! `else` without a preceding `if`). They run only on authored containers —
//! never on dynamic-definition expansions, whose bodies belong to the definition
//! site.

use crate::support::ScriptProperty;
use crate::types::{Diagnostic, DiagnosticCode};
use pdx_text::TextRange;

fn push(
    diagnostics: &mut Vec<Diagnostic>,
    code: DiagnosticCode,
    range: TextRange,
    message: String,
) {
    diagnostics.push(Diagnostic::new(code, code.severity(), range, message));
}

fn key_is(property: &ScriptProperty, key: &str) -> bool {
    property.key.eq_ignore_ascii_case(key)
}

/// Returns true when the key is a boolean logic container usable in trigger context.
pub(crate) fn is_boolean_container_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("AND")
        || key.eq_ignore_ascii_case("OR")
        || key.eq_ignore_ascii_case("NOT")
}

/// Returns true when the key participates in if/else_if/else branch chains.
pub(crate) fn is_conditional_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("if")
        || key.eq_ignore_ascii_case("else_if")
        || key.eq_ignore_ascii_case("else")
}

/// Lints AND/OR/NOT shapes: empty containers, single-child wrappers, and the
/// NOT-with-multiple-conditions reading trap.
pub(crate) fn lint_boolean_container(property: &ScriptProperty, diagnostics: &mut Vec<Diagnostic>) {
    let conditions = property.block.len();
    if conditions == 0 {
        let meaning = if property.key.eq_ignore_ascii_case("OR") {
            "always false"
        } else {
            "always true"
        };
        push(
            diagnostics,
            DiagnosticCode::LogicalContainer,
            property.key_range,
            format!(
                "empty `{}` container is {meaning}; drop the container or add conditions",
                property.key
            ),
        );
        return;
    }
    if property.key.eq_ignore_ascii_case("NOT") && conditions > 1 {
        push(
            diagnostics,
            DiagnosticCode::LogicalContainer,
            property.key_range,
            "`NOT` with multiple conditions is true only when none of them hold (an AND of NOTs); it is not \"not all of them hold\". Write `AND = { NOT = { ... } ... }` to state the intended reading".to_string(),
        );
        return;
    }
    if !property.key.eq_ignore_ascii_case("NOT") && conditions == 1 {
        push(
            diagnostics,
            DiagnosticCode::LogicalContainer,
            property.key_range,
            format!(
                "`{}` with a single condition is equivalent to the condition itself; the wrapper can be removed",
                property.key
            ),
        );
    }
}

fn limit_child(property: &ScriptProperty) -> Option<&ScriptProperty> {
    property.block.iter().find(|child| key_is(child, "limit"))
}

fn limit_is_constant(property: &ScriptProperty) -> Option<bool> {
    let limit = limit_child(property)?;
    if limit.block.len() != 1 {
        return None;
    }
    let child = &limit.block[0];
    if !key_is(child, "always") {
        return None;
    }
    let (value, _) = child.scalar.as_ref()?;
    if value.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if value.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

/// Lints `if`/`else_if`/`else` blocks: empty bodies, missing effect-side
/// `limit`, and statically constant limits. `effect_like` selects the
/// effect-side message; trigger-side `limit` cardinality is already owned by
/// the semantic rules.
pub(crate) fn lint_conditional_block(
    property: &ScriptProperty,
    effect_like: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let is_else = key_is(property, "else");
    if !is_else && property.block.is_empty() && property.bare_values.is_empty() {
        push(
            diagnostics,
            DiagnosticCode::EmptyBlock,
            property.key_range,
            format!("`{}` block has an empty body", property.key),
        );
    }
    if is_else {
        return;
    }
    if effect_like && limit_child(property).is_none() {
        let behavior = if key_is(property, "if") {
            "executes its body unconditionally"
        } else {
            "is taken unconditionally once reached"
        };
        push(
            diagnostics,
            DiagnosticCode::MissingLimit,
            property.key_range,
            format!(
                "`{}` without `limit` {behavior}; the condition belongs in a `limit` block",
                property.key
            ),
        );
    }
    match limit_is_constant(property) {
        Some(true) => push(
            diagnostics,
            DiagnosticCode::ConstantCondition,
            property.key_range,
            format!(
                "`limit` of this `{}` is always true; the branch wrapper is redundant",
                property.key
            ),
        ),
        Some(false) => push(
            diagnostics,
            DiagnosticCode::ConstantCondition,
            property.key_range,
            format!(
                "`limit` of this `{}` is always false; the branch never executes",
                property.key
            ),
        ),
        None => {}
    }
}

/// Lints branch-chain ordering: `else`/`else_if` must either directly follow
/// an `if`/`else_if` sibling or be nested inside an `if`/`else_if` block.
pub(crate) fn lint_conditional_siblings(
    properties: &[&ScriptProperty],
    enclosing_key: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // EU4 binds `else` in two valid positions: after an `if`/`else_if`
    // sibling, or as a child of an `if` block (`if = { limit = {...} ...
    // else = {...} }`), so a nested position never orphans the branch.
    let nested_valid = enclosing_key
        .is_some_and(|key| key.eq_ignore_ascii_case("if") || key.eq_ignore_ascii_case("else_if"));
    let mut previous: Option<&ScriptProperty> = None;
    for property in properties {
        let current = property.key.as_ref();
        // Only block-valued branches are logic: `else = "..."` is a quoted
        // payload argument (e.g. `add_country_modifier_for_age`'s else clause),
        // not a branch, and never orphans.
        if (current.eq_ignore_ascii_case("else") || current.eq_ignore_ascii_case("else_if"))
            && property.block_range.is_some()
            && !nested_valid
            && previous.is_none_or(|previous| {
                let key = previous.key.as_ref();
                !(key.eq_ignore_ascii_case("if") || key.eq_ignore_ascii_case("else_if"))
            })
        {
            push(
                diagnostics,
                DiagnosticCode::OrphanElse,
                property.key_range,
                format!(
                    "orphan `{current}`: it must directly follow an `if`/`else_if` block or be nested inside one",
                ),
            );
        }
        previous = Some(property);
    }
}
