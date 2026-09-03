//! Scope-attribution diagnostics for effects that apply typed definitions.
//!
//! Some effects apply a workspace definition — for EU4, `add_country_modifier`
//! and `add_province_modifier` apply an `event_modifier` — and every attribute
//! inside that definition's body is itself scope-attributed by the semantic
//! rules. Modifier attributes partition into exactly three mutually exclusive
//! classes — country, province, and unit — with no unattributed class among
//! known keys. This check reads the effect's own rule scope, the typed `name`
//! child rule, the retained attribute keys of the referenced definition, and
//! the attribute scope rows, entirely from rule data, and reports cross-class
//! applications on the `name` value:
//!
//! - a country application accepts unit-class attributes silently (the game
//!   propagates them to the country's units) and reports province-class ones;
//! - a province application accepts country-class attributes — reported at
//!   information severity — and reports unit-class ones;
//! - a unit application accepts only unit-class attributes.
//!
//! Every report is information severity: the game still loads cross-class
//! applications, so the message records the class mismatch instead of
//! rejecting the file. Attributes without a scope attribution, dynamic
//! `$param$` names, and unresolved names are never reported. A typed child may
//! name more than one candidate kind (for EU4, `event_modifier` and
//! `static_modifier`); the first kind that resolves retained definition
//! attributes decides.

use std::collections::BTreeSet;
use std::sync::Arc;

use pdx_engine::hir::DefinitionAttributes;
use pdx_engine::{AnalysisSnapshot, DocumentSource};
use pdx_rules::{RuleShape, ValueMatcher};

use crate::support::{ParsedInput, ScriptProperty};
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode};

/// Returns scope-attribution diagnostics for modifier-applying effects in
/// `input`.
pub(crate) fn modifier_scope_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != pdx_parser::FileFormat::Script {
        return Ok(Vec::new());
    }
    let crate::support::ParsedContent::Text(parsed) = &input.parsed;
    let contexts = callable_body_contexts(snapshot);
    let mut diagnostics = Vec::new();
    for property in crate::support::script_properties(input, parsed.root()) {
        cancellation.checkpoint()?;
        report_property(snapshot, &property, &contexts, &mut diagnostics);
    }
    Ok(diagnostics)
}

/// Body contexts of all macro-enabled types: the callable contexts in which
/// effect-like keys carry semantic rows.
fn callable_body_contexts(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .values()
        .filter_map(|descriptor| {
            descriptor
                .scripted_macro
                .as_ref()
                .filter(|macro_descriptor| macro_descriptor.macro_enabled)
                .map(|macro_descriptor| macro_descriptor.body_context.to_ascii_lowercase())
        })
        .collect()
}

fn report_property(
    snapshot: &AnalysisSnapshot,
    property: &ScriptProperty,
    contexts: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // The effect key must resolve, in one callable context, to rows that all
    // agree on exactly one allowed scope; anything else stays untouched.
    if let Some((context, scope)) = single_effect_scope(snapshot, contexts, &property.key) {
        for child in &property.block {
            report_application(
                snapshot,
                &context,
                &property.key,
                &scope,
                child,
                diagnostics,
            );
        }
    }
    for child in &property.block {
        report_property(snapshot, child, contexts, diagnostics);
    }
}

fn report_application(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_key: &str,
    scope: &str,
    child: &ScriptProperty,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((name, value_range)) = child.scalar.as_ref() else {
        return;
    };
    if name.contains('$') || name.is_empty() {
        return;
    }
    let candidate_kinds = typed_children(snapshot, context, parent_key, &child.key);
    if candidate_kinds.is_empty() {
        return;
    }
    let Some((attribute_keys, attribute_context)) = candidate_kinds
        .iter()
        .filter_map(|kind| {
            let attribute_keys = definition_attribute_keys(snapshot, kind, name)?;
            let attribute_context = snapshot
                .rules()
                .model()
                .semantic
                .type_descriptors
                .get(kind.as_str())
                .and_then(|descriptor| descriptor.body_context.clone())?;
            Some((attribute_keys, attribute_context))
        })
        .next()
    else {
        return;
    };
    let (mut country, mut province, mut unit) = (Vec::new(), Vec::new(), Vec::new());
    for key in &attribute_keys {
        match attribute_scope(snapshot, &attribute_context, key) {
            AttributeScope::Country => country.push(key.clone()),
            AttributeScope::Province => province.push(key.clone()),
            AttributeScope::Unit => unit.push(key.clone()),
            AttributeScope::Other | AttributeScope::Unknown => {}
        }
    }
    let application = scope.to_ascii_lowercase();
    let mut report = |class: &str, keys: &[String], allowed: bool| {
        if keys.is_empty() {
            return;
        }
        let qualifier = if allowed { "" } else { "unexpected " };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::ModifierScopeMismatch,
            DiagnosticCode::ModifierScopeMismatch.severity(),
            *value_range,
            format!(
                "modifier `{name}` applies {}{class}-class attributes ({}) in {application} scope",
                qualifier,
                keys.join(", ")
            ),
        ));
    };
    match application.as_str() {
        "province" => {
            report("country", &country, true);
            report("unit", &unit, false);
        }
        "country" => {
            report("province", &province, false);
        }
        "unit" => {
            report("country", &country, false);
            report("province", &province, false);
        }
        _ => {}
    }
}

/// The single scope every row of `key` in one callable context allows, when
/// the rows agree on exactly one. Only block-shaped rows participate: an
/// effect that applies a definition takes a block.
fn single_effect_scope(
    snapshot: &AnalysisSnapshot,
    contexts: &BTreeSet<String>,
    key: &str,
) -> Option<(String, String)> {
    let mut found: Option<(String, String)> = None;
    for context in contexts {
        let rows = matching_rows(snapshot, context, &[], key)
            .into_iter()
            .filter(|rule| matches!(rule.shape, RuleShape::Node | RuleShape::QuotedScript))
            .collect::<Vec<_>>();
        if rows.is_empty() || rows.iter().any(|rule| rule.allowed_scopes.is_empty()) {
            continue;
        }
        let scopes = rows
            .iter()
            .flat_map(|rule| rule.allowed_scopes.iter())
            .map(|scope| scope.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        if scopes.len() != 1 {
            continue;
        }
        let scope = scopes.into_iter().next().expect("one scope");
        match &found {
            Some((_, existing)) if !existing.eq_ignore_ascii_case(&scope) => return None,
            _ => found = Some((context.clone(), scope)),
        }
    }
    found
}

/// The workspace types a scalar child of `parent_key` can name, from the typed
/// child rules (`name = { type = event_modifier }` in the rule source). More
/// than one kind may be declared; the caller picks the first that resolves.
fn typed_children(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_key: &str,
    child_key: &str,
) -> Vec<String> {
    let parent_path = [Arc::<str>::from(parent_key.to_ascii_lowercase())];
    matching_rows(snapshot, context, &parent_path, child_key)
        .into_iter()
        .filter_map(|rule| match &rule.value {
            ValueMatcher::Type(kind) => Some(kind.clone()),
            _ => None,
        })
        .collect()
}

/// Attribute keys of the uniquely active definition, overlay first.
fn definition_attribute_keys(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
) -> Option<Vec<String>> {
    let mut overlay_hits: Vec<DefinitionAttributes> = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        overlay_hits.extend(
            hir.definition_attributes()
                .iter()
                .filter(|attributes| {
                    attributes.kind.eq_ignore_ascii_case(kind)
                        && attributes.name.eq_ignore_ascii_case(name)
                })
                .cloned(),
        );
    }
    if overlay_hits.len() > 1 {
        return None;
    }
    if let Some(attributes) = overlay_hits.pop() {
        return Some(attributes.attribute_keys.clone());
    }
    let definition = snapshot.index().active_definition(kind, name)?;
    let hidden_by_overlay = snapshot.documents().values().any(|document| {
        document.source() == DocumentSource::Overlay
            && document.path().is_some_and(|path| {
                snapshot
                    .source_files()
                    .get(&definition.file_id)
                    .is_some_and(|file| file.physical_path == path)
            })
    });
    if hidden_by_overlay {
        return None;
    }
    snapshot
        .index()
        .active_definition_attributes(kind, name)
        .map(|attributes| attributes.attribute_keys.clone())
}

enum AttributeScope {
    Country,
    Province,
    Unit,
    Other,
    Unknown,
}

fn attribute_scope(snapshot: &AnalysisSnapshot, context: &str, key: &str) -> AttributeScope {
    let rows = matching_rows(snapshot, context, &[], key);
    if rows.is_empty() || rows.iter().any(|rule| rule.allowed_scopes.is_empty()) {
        return AttributeScope::Unknown;
    }
    let scopes = rows
        .iter()
        .flat_map(|rule| rule.allowed_scopes.iter())
        .map(|scope| scope.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if scopes.len() != 1 {
        return AttributeScope::Unknown;
    }
    let scope = scopes.iter().next().expect("one scope");
    if scope.eq_ignore_ascii_case("country") {
        AttributeScope::Country
    } else if scope.eq_ignore_ascii_case("province") {
        AttributeScope::Province
    } else if scope.eq_ignore_ascii_case("unit") {
        AttributeScope::Unit
    } else {
        AttributeScope::Other
    }
}

fn matching_rows<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[Arc<str>],
    key: &str,
) -> Vec<&'a pdx_rules::SemanticRule> {
    crate::semantic::semantic_rules_for_container_key(snapshot, context, parent_path, key)
        .into_iter()
        .filter(|rule| {
            crate::semantic::semantic_rule_key_matches(snapshot, rule, parent_path, key)
                && !matches!(rule.shape, RuleShape::LeafValue | RuleShape::ValueClause)
        })
        .collect()
}
