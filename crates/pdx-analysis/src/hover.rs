use std::collections::BTreeSet;
use std::sync::Arc;

use crate::completion::*;
use crate::resolution::*;
use crate::semantic::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_parser::{CstKind, CstNode};
use pdx_rules::{KeyMatcher, RuleShape, SymbolResolutionPolicy, ValueMatcher};
use pdx_text::{TextRange, TextSize};

/// Computes hover information without reading the full contents of another file.
#[must_use]
pub fn hover(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Option<Hover> {
    uncancelled(hover_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Computes hover information with cooperative cancellation checkpoints.
pub fn hover_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Hover>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(None);
    };
    if let Some((definition, reference)) = local_parameter_target(&input, position) {
        // The owner name lives directly on the HIR definition with the same range; going through
        // `semantic_data` here would lower the whole file just to recover one spelling.
        let owner_name = input.hir.as_deref().and_then(|hir| {
            hir.definitions()
                .iter()
                .find(|candidate| candidate.range == definition.owner_range)
                .map(|candidate| candidate.name.clone())
        });
        let occurrences = input
            .hir
            .as_deref()
            .map(|hir| {
                hir.parameter_references_for_owner(definition.owner_range)
                    .filter(|reference| reference.name.eq_ignore_ascii_case(&definition.name))
                    .count()
            })
            .unwrap_or(0);
        let optional = input.hir.as_deref().is_some_and(|hir| {
            !hir.parameter_is_required(definition.owner_range, &definition.name)
        });
        let syntax = match reference.kind {
            pdx_engine::hir::HirParameterReferenceKind::Substitution => "substitution",
            pdx_engine::hir::HirParameterReferenceKind::KeySubstitution => "key substitution",
            pdx_engine::hir::HirParameterReferenceKind::OpaqueTextSubstitution => {
                "opaque text substitution"
            }
            pdx_engine::hir::HirParameterReferenceKind::Conditional => "conditional",
        };
        let owner = owner_name.map_or_else(
            || "scripted definition".to_owned(),
            |name| format!("scripted definition {}", code_span(&name)),
        );
        return Ok(Some(Hover {
            contents: format!(
                "### parameter {}\n\n- Local to {owner}; inferred from its first use\n- Presence: `{}`\n- Syntax: `{syntax}`\n- Occurrences in owner: {occurrences}",
                code_span(&definition.name),
                if optional {
                    "optional"
                } else {
                    "required/inferred"
                },
            ),
            range: Some(reference.name_range),
        }));
    }
    let range = word_range(&input.source, position);
    let Some(word) = input
        .source_text(range)
        .map(|word| word.trim_matches('"').to_owned())
    else {
        return Ok(None);
    };
    if word.is_empty() {
        return Ok(None);
    }
    let semantic = semantic_data(snapshot, &input);
    let mut references = semantic.references.iter().filter(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    });
    if let Some(first) = references.next() {
        let mut best = hover_for_symbol(snapshot, &first.kind, &first.name, range, cancellation)?;
        if !best.has_localisation_preview {
            for reference in references {
                let hover = hover_for_symbol(
                    snapshot,
                    &reference.kind,
                    &reference.name,
                    range,
                    cancellation,
                )?;
                if hover.has_localisation_preview {
                    best = hover;
                    break;
                }
            }
        }
        return Ok(Some(best.into_hover_with_range(range)));
    }
    if let Some(definition) = semantic.definitions.iter().find(|definition| {
        definition.document.as_ref() == Some(document)
            && contains(definition.symbol.selection_range, position)
    }) {
        return Ok(Some(
            hover_for_symbol(
                snapshot,
                &definition.kind,
                &definition.name,
                range,
                cancellation,
            )?
            .into_hover_with_range(range),
        ));
    }
    cancellation.checkpoint()?;
    if let Some(details) = semantic_rule_hover_at(snapshot, &input, position, cancellation)? {
        return Ok(Some(Hover {
            contents: format!("### PDX property {}\n\n{details}", code_span(&word)),
            range: Some(range),
        }));
    }
    if let Some(details) = semantic_value_hover_at(snapshot, &input, position, cancellation)? {
        return Ok(Some(Hover {
            contents: format!("### PDX value {}\n\n{details}", code_span(&word)),
            range: Some(range),
        }));
    }
    if is_property_key_at(&input, position) {
        if known_keys(snapshot)
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&word))
        {
            let contents = semantic_rule_documentation(snapshot, &word).map_or_else(
                || format!("### PDX property {}", code_span(&word)),
                |details| format!("### PDX property {}\n\n{details}", code_span(&word)),
            );
            return Ok(Some(Hover {
                contents,
                range: Some(range),
            }));
        }
        // The key may still be covered by a non-exact first-party matcher (type member, enum
        // member, date, or dynamic set). Surface that provenance instead of returning nothing.
        if let Some(hint) = semantic_pattern_rule_hint(snapshot, &word) {
            return Ok(Some(Hover {
                contents: format!("### PDX property {}\n\n{}", code_span(&word), hint),
                range: Some(range),
            }));
        }
    }
    // Do not manufacture a tooltip for every bare word in a script or comment.  A hover is only
    // useful when the parser/HIR/rules have established a semantic role for the token.
    Ok(None)
}

pub(crate) fn is_property_key_at(input: &ParsedInput, position: TextSize) -> bool {
    input.hir.as_deref().is_some_and(|hir| {
        hir.properties()
            .iter()
            .any(|property| contains(property.key_range, position))
    })
}

pub(crate) fn semantic_rule_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
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
    let candidates = semantic_rules_for_completion(snapshot, &context)
        .into_iter()
        .filter(|candidate| {
            !matches!(candidate.rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &property.key,
                )
        })
        .collect::<Vec<_>>();
    Ok((!candidates.is_empty()).then(|| semantic_rule_hover_for_candidates(snapshot, &candidates)))
}

pub(crate) fn semantic_value_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
    let Some(context) =
        semantic_completion_context_with_cancellation(snapshot, input, position, cancellation)?
    else {
        return Ok(None);
    };
    let Some(property) = context.property.as_ref() else {
        return Ok(None);
    };
    let Some((value, value_range)) = property.scalar.as_ref() else {
        return Ok(None);
    };
    if !contains(*value_range, position) {
        return Ok(None);
    }
    let candidates = semantic_rules_for_completion(snapshot, &context)
        .into_iter()
        .filter(|candidate| {
            matches!(candidate.rule.shape, RuleShape::Leaf)
                && semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &property.key,
                )
                && candidate
                    .rule
                    .operator
                    .as_deref()
                    .is_none_or(|operator| property.operator.as_deref() == Some(operator))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let accepted = candidates.iter().any(|candidate| {
        semantic_scope_allows(candidate.rule, candidate.scope)
            && semantic_property_matches(snapshot, candidate.rule, property, candidate.scope)
    });
    Ok(Some(format!(
        "- property: `{}`\n- value: `{}`\n- validation: `{}`\n\n{}",
        property.key,
        value,
        if accepted {
            "accepted"
        } else {
            "does not match"
        },
        semantic_rule_hover_for_candidates(snapshot, &candidates)
    )))
}

pub(crate) fn semantic_rule_hover_for_candidates(
    snapshot: &AnalysisSnapshot,
    candidates: &[SemanticCompletionRule<'_, '_>],
) -> String {
    // Stable rule ids and source provenance identify declarations, not necessarily distinct
    // meanings.  The first-party source can repeat one semantic rule for many generated members;
    // keep those rows explainable in diagnostics, but do not render the same hover 226 times.
    let mut unique_candidates: Vec<&SemanticCompletionRule<'_, '_>> =
        Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !unique_candidates
            .iter()
            .any(|known| semantic_hover_candidate_equivalent(known, candidate))
        {
            unique_candidates.push(candidate);
        }
    }
    let candidates = unique_candidates.as_slice();
    if candidates.len() > 1 {
        let shared_documentation = shared_semantic_hover_documentation(candidates);
        let summaries = candidates
            .iter()
            .map(|candidate| {
                semantic_hover_candidate_summary(
                    snapshot,
                    candidate,
                    shared_documentation.is_none(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut sections = vec![format!(
            "#### Possible meanings ({})\n\n{summaries}",
            candidates.len()
        )];
        if let Some(documentation) = shared_documentation {
            sections.push(format!(
                "#### Documentation\n\n{}",
                truncate_documentation(&documentation)
            ));
        }
        return sections.join("\n\n");
    }

    let Some(candidate) = candidates.first() else {
        return String::new();
    };
    let rule = candidate.rule;
    let mut sections = vec![semantic_hover_candidate_details(snapshot, candidate).join("\n")];
    let cardinality = semantic_hover_cardinality_details(rule);
    if !cardinality.is_empty() {
        sections.push(format!("#### Constraints\n\n{}", cardinality.join("\n")));
    }
    if !rule.documentation.is_empty() {
        sections.push(format!(
            "#### Documentation\n\n{}",
            truncate_documentation(&rule.documentation)
        ));
    }
    sections.join("\n\n")
}

fn semantic_hover_candidate_summary(
    snapshot: &AnalysisSnapshot,
    candidate: &SemanticCompletionRule<'_, '_>,
    include_documentation: bool,
) -> String {
    let mut details = semantic_hover_candidate_details(snapshot, candidate);
    details.extend(semantic_hover_cardinality_details(candidate.rule));
    if include_documentation && !candidate.rule.documentation.is_empty() {
        details.push(format!(
            "- documentation: {}",
            truncate_documentation(&candidate.rule.documentation)
        ));
    }
    details.join("\n")
}

fn semantic_hover_candidate_details(
    snapshot: &AnalysisSnapshot,
    candidate: &SemanticCompletionRule<'_, '_>,
) -> Vec<String> {
    let rule = candidate.rule;
    let mut details = vec![format!(
        "- value: {}",
        semantic_rule_hover_value_label(rule)
    )];
    if !rule.allowed_scopes.is_empty() {
        let allowed = rule
            .allowed_scopes
            .iter()
            .map(|scope| format!("`{scope}`"))
            .collect::<Vec<_>>()
            .join(", ");
        if semantic_scope_allows(rule, candidate.scope) {
            details.push(format!("- valid scopes: {allowed}"));
        } else {
            details.push(format!(
                "- unavailable in current scope `{}`; valid scopes: {allowed}",
                candidate.scope.current
            ));
        }
    }
    if (rule.push_scope.is_some() || !rule.replace_scope.is_empty()) && {
        let child_scope = semantic_child_scope(snapshot, candidate.scope, rule);
        !candidate
            .scope
            .current
            .eq_ignore_ascii_case(&child_scope.current)
    } {
        let child_scope = semantic_child_scope(snapshot, candidate.scope, rule);
        details.push(format!(
            "- scope transition: `{}` → `{}`",
            candidate.scope.current, child_scope.current
        ));
    }
    details
}

fn semantic_hover_cardinality_details(rule: &pdx_rules::SemanticRule) -> Vec<String> {
    let mut details = Vec::new();
    if rule.required {
        details.push("- required".to_owned());
    }
    if let Some(min) = rule.min_occurs.filter(|min| *min > 0)
        && (!rule.required || min > 1)
    {
        details.push(format!("- at least {min}"));
    }
    if let Some(max) = rule.max_occurs.filter(|max| *max != 1) {
        details.push(format!("- at most {max}"));
    }
    details
}

fn shared_semantic_hover_documentation(
    candidates: &[&SemanticCompletionRule<'_, '_>],
) -> Option<Vec<String>> {
    let first = candidates.first()?.rule.documentation.clone();
    (!first.is_empty()
        && candidates
            .iter()
            .all(|candidate| candidate.rule.documentation == first))
    .then_some(first)
}

fn semantic_rule_hover_value_label(rule: &pdx_rules::SemanticRule) -> String {
    match rule.shape {
        RuleShape::Node => "block".to_owned(),
        RuleShape::QuotedScript => "quoted script".to_owned(),
        RuleShape::ValueClause => "value clause".to_owned(),
        RuleShape::Leaf | RuleShape::LeafValue => semantic_value_hover_label(&rule.value),
    }
}

fn semantic_hover_candidate_equivalent(
    left: &SemanticCompletionRule<'_, '_>,
    right: &SemanticCompletionRule<'_, '_>,
) -> bool {
    left.parent_path == right.parent_path
        && left.scope == right.scope
        && left.rule.context == right.rule.context
        && left.rule.parent_path == right.rule.parent_path
        && left.rule.key == right.rule.key
        && left.rule.operator == right.rule.operator
        && left.rule.value == right.rule.value
        && left.rule.shape == right.rule.shape
        && left.rule.child_context == right.rule.child_context
        // alternative_id, id, source_file, and line describe source declarations rather than
        // another user-visible semantic interpretation.
        && left.rule.severity == right.rule.severity
        && left.rule.required == right.rule.required
        && left.rule.deprecated == right.rule.deprecated
        && left.rule.documentation == right.rule.documentation
        && left.rule.allowed_scopes == right.rule.allowed_scopes
        && left.rule.push_scope == right.rule.push_scope
        && left.rule.replace_scope == right.rule.replace_scope
        && left.rule.min_occurs == right.rule.min_occurs
        && left.rule.strict_min == right.rule.strict_min
        && left.rule.max_occurs == right.rule.max_occurs
}

/// Renders a value-matcher description. Some labels embed their own inline code
/// spans (for example ``dynamic value set `country_flag` ``), so callers must
/// not wrap the result in another code span — the nested backticks would break
/// apart in the rendered Markdown.
pub(crate) fn semantic_value_hover_label(matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar => "any scalar".to_owned(),
        ValueMatcher::Exact(value) => format!("exact `{value}`"),
        ValueMatcher::Bool => "bool (`yes` / `no`)".to_owned(),
        ValueMatcher::Int { min, max } => semantic_numeric_hover_label("integer", *min, *max),
        ValueMatcher::Float { min, max } => {
            let bounds = match (min.as_deref(), max.as_deref()) {
                (Some(min), Some(max)) => format!(" in [{min}, {max}]"),
                (Some(min), None) => format!(" >= {min}"),
                (None, Some(max)) => format!(" <= {max}"),
                (None, None) => String::new(),
            };
            format!("float{bounds}")
        }
        ValueMatcher::Date => "date (`YYYY.MM.DD`)".to_owned(),
        ValueMatcher::Type(value) => format!("symbol type `{value}`"),
        ValueMatcher::Enum(value) => format!("enum `{value}`"),
        ValueMatcher::Scope(value) => value
            .as_deref()
            .map_or_else(|| "scope".to_owned(), |value| format!("scope `{value}`")),
        ValueMatcher::Localisation => "localisation key".to_owned(),
        ValueMatcher::Filepath => "filepath".to_owned(),
        ValueMatcher::Dynamic(value) => format!("dynamic value `{value}`"),
        ValueMatcher::DynamicSet(value) => format!("dynamic value set `{value}`"),
        ValueMatcher::Opaque(value) => format!("opaque `{value}`"),
    }
}

pub(crate) fn semantic_numeric_hover_label<T: std::fmt::Display>(
    kind: &str,
    min: Option<T>,
    max: Option<T>,
) -> String {
    let bounds = match (min, max) {
        (Some(min), Some(max)) => format!(" in [{min}, {max}]"),
        (Some(min), None) => format!(" >= {min}"),
        (None, Some(max)) => format!(" <= {max}"),
        (None, None) => String::new(),
    };
    format!("{kind}{bounds}")
}

pub(crate) fn semantic_rule_documentation(
    snapshot: &AnalysisSnapshot,
    key: &str,
) -> Option<String> {
    let mut rules = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .filter(|rule| match &rule.key {
            KeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
            _ => false,
        })
        .collect::<Vec<_>>();
    rules.sort_by_key(|rule| (&rule.context, &rule.parent_path, &rule.id));
    let rule = rules.into_iter().find(|rule| {
        !rule.documentation.is_empty()
            || rule.required
            || rule.min_occurs.is_some_and(|min| min > 0)
            || rule.max_occurs.is_some_and(|max| max != 1)
            || !rule.allowed_scopes.is_empty()
    })?;
    semantic_rule_documentation_for_rule(rule)
}

/// Renders first-party documentation lines, truncating the total so a pathological declaration
/// cannot produce an unbounded tooltip.
fn truncate_documentation(documentation: &[String]) -> String {
    const MAX_DOCUMENTATION_CHARS: usize = 1_200;
    let mut rendered = String::new();
    let mut overflow = false;
    for line in documentation {
        let line = truncate_hover_text(line);
        if rendered.chars().count() + line.chars().count() > MAX_DOCUMENTATION_CHARS {
            overflow = true;
            break;
        }
        if !rendered.is_empty() {
            rendered.push_str("  \n");
        }
        rendered.push_str(&line);
    }
    if overflow || rendered.chars().count() > MAX_DOCUMENTATION_CHARS {
        rendered.push_str("  \n…");
    }
    rendered
}

pub(crate) fn semantic_rule_documentation_for_rule(
    rule: &pdx_rules::SemanticRule,
) -> Option<String> {
    let mut sections = Vec::new();
    if !rule.documentation.is_empty() {
        sections.push(format!(
            "#### Documentation\n\n{}",
            truncate_documentation(&rule.documentation)
        ));
    }

    let mut constraints = Vec::new();
    if rule.required {
        constraints.push("- required".to_owned());
    }
    if let Some(min) = rule.min_occurs.filter(|min| *min > 0)
        && (!rule.required || min > 1)
    {
        constraints.push(format!("- at least {min}"));
    }
    if let Some(max) = rule.max_occurs.filter(|max| *max != 1) {
        constraints.push(format!("- at most {max}"));
    }
    if !rule.allowed_scopes.is_empty() {
        constraints.push(format!(
            "- scopes: {}",
            rule.allowed_scopes
                .iter()
                .map(|scope| format!("`{scope}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !constraints.is_empty() {
        sections.push(format!("#### Constraints\n\n{}", constraints.join("\n")));
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}
/// Maximum number of candidate paths rendered before the list is truncated.
const MAX_HOVER_CANDIDATES: usize = 8;

/// Structured result of one symbol hover. Rendering to Markdown is deferred so callers can
/// branch on resolution facts (for example preferring a variant with a localisation preview)
/// without string-matching rendered headings.
pub(crate) struct SymbolHover {
    contents: String,
    has_localisation_preview: bool,
}

impl SymbolHover {
    fn into_hover_with_range(self, range: TextRange) -> Hover {
        Hover {
            contents: self.contents,
            range: Some(range),
        }
    }
}

pub(crate) fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    _range: TextRange,
    cancellation: &CancellationToken,
) -> Result<SymbolHover, Cancelled> {
    let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
    let policy = symbol_resolution_policy(snapshot, kind);
    let mut sections = vec![format!("### {} {}", kind, code_span(name))];
    let mut has_localisation_preview = false;
    if candidates.is_empty() {
        sections.push(format!("#### unresolved {kind} symbol"));
    } else {
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        let active = match policy {
            SymbolResolutionPolicy::ReplaceBySymbol => candidates
                .iter()
                .filter(|candidate| candidate.priority == highest)
                .collect::<Vec<_>>(),
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => {
                if candidates.len() == 1 {
                    vec![&candidates[0]]
                } else {
                    Vec::new()
                }
            }
        };
        if active.len() == 1 {
            let definition = active[0];
            sections.push(format!(
                "#### Resolved definition\n\n- Source root: {}\n- Defined in: `{}`",
                symbol_source_root(snapshot, &definition.location),
                symbol_location_path(&definition.location),
            ));
            let shadowed = candidates
                .iter()
                .filter(|candidate| {
                    !same_location(&candidate.location, &definition.location)
                        && candidate.priority < definition.priority
                })
                .collect::<Vec<_>>();
            if !shadowed.is_empty() {
                sections.push(format!(
                    "#### Shadowed definitions:\n\n{}",
                    shadowed
                        .into_iter()
                        .map(|candidate| format!(
                            "- {}: `{}`",
                            symbol_source_root(snapshot, &candidate.location),
                            symbol_location_path(&candidate.location),
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            if let Some((language, value)) =
                symbol_localisation_preview(snapshot, kind, name, definition, cancellation)?
            {
                has_localisation_preview = true;
                sections.push(format!(
                    "#### Localisation preview\n\n- Localisation{}: \"{}\"",
                    language
                        .as_deref()
                        .map_or_else(String::new, |language| format!(" ({language})")),
                    value
                ));
            }
            if let Some(summary) = macro_definition_summary(snapshot, kind, name) {
                let mut signature = macro_signature_hover(&summary);
                if crate::semantic::scripted_macro_type(snapshot, kind) {
                    signature.push('\n');
                    signature.push_str(&crate::macro_contracts::contract_hover_line(
                        snapshot, kind, name,
                    ));
                }
                sections.push(signature);
            }
        } else {
            // Ambiguous symbols still deserve a preview when any candidate carries localisation
            // text; that is exactly the case where the user most wants to see the translations.
            if kind.eq_ignore_ascii_case("localisation") {
                for candidate in &candidates {
                    if let Some((language, value)) = localisation_preview(snapshot, candidate) {
                        if value.is_empty() {
                            continue;
                        }
                        has_localisation_preview = true;
                        sections.push(format!(
                            "#### Localisation preview\n\n- Localisation{}: \"{}\"",
                            language
                                .as_deref()
                                .map_or_else(String::new, |language| format!(" ({language})")),
                            value
                        ));
                        break;
                    }
                }
            } else {
                for candidate in &candidates {
                    if let Some((language, value)) =
                        symbol_localisation_preview(snapshot, kind, name, candidate, cancellation)?
                    {
                        has_localisation_preview = true;
                        sections.push(format!(
                            "#### Localisation preview\n\n- Localisation{}: \"{}\"",
                            language
                                .as_deref()
                                .map_or_else(String::new, |language| format!(" ({language})")),
                            value
                        ));
                        break;
                    }
                }
            }
            let shown = candidates.len().min(MAX_HOVER_CANDIDATES);
            let mut lines = candidates
                .iter()
                .take(shown)
                .map(|candidate| {
                    format!(
                        "- {}: `{}`",
                        symbol_source_root(snapshot, &candidate.location),
                        symbol_location_path(&candidate.location),
                    )
                })
                .collect::<Vec<_>>();
            if candidates.len() > shown {
                lines.push(format!("- … and {} more", candidates.len() - shown));
            }
            sections.push(format!("#### ambiguous {kind} symbol"));
            sections.push(format!("#### Candidates:\n\n{}", lines.join("\n")));
        }
    }
    Ok(SymbolHover {
        contents: sections.join("\n\n"),
        has_localisation_preview,
    })
}

fn macro_signature_hover(summary: &pdx_engine::MacroDefinitionSummary) -> String {
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

pub(crate) fn localisation_preview(
    snapshot: &AnalysisSnapshot,
    definition: &ResolutionDefinition,
) -> Option<(Option<String>, String)> {
    if let Some(file) = definition.location.file
        && let Some(preview) = snapshot.localisation_preview(file, definition.location.range)
    {
        return Some((preview.language.clone(), preview.value.clone()));
    }
    let input = definition
        .location
        .document
        .as_ref()
        .and_then(|document| input_for_document(snapshot, document))
        .or_else(|| {
            definition
                .location
                .file
                .and_then(|file| input_for_source_file(snapshot, file))
        })?;
    let ParsedContent::Text(parsed) = &input.parsed;
    let entry = find_cst_node(
        parsed.root(),
        CstKind::LocalisationEntry,
        definition.location.range,
    )?;
    let value_node = entry.children().find(|child| {
        matches!(
            child.kind(),
            CstKind::LocalisationString | CstKind::UnquotedValue
        )
    })?;
    let raw = parsed.text(value_node.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw);
    let value = truncate_hover_text(value);
    if value.is_empty() {
        return None;
    }
    let mut language = None;
    for node in parsed.root().children() {
        if node.range().start() > entry.range().start() {
            break;
        }
        if node.kind() == CstKind::LanguageHeader
            && let Some(value) = node
                .children()
                .find(|child| child.kind() == CstKind::LocalisationKey)
                .and_then(|child| parsed.text(child.range()))
        {
            language = Some(value.trim().to_owned());
        }
    }
    Some((language, value))
}

/// Finds the first non-empty localisation attached to a non-localisation symbol definition.
///
/// Type-instance localisation mappings are indexed as ordinary localisation references at the
/// instance's source range.  Looking those references up from the resolved definition lets a
/// hover over `event = foo.1` (or another typed symbol use) show the same preview as hovering its
/// generated localisation key. Type descriptors may also use the implicit same-name convention
/// without a localisation-binding row. Cache-only roots retain required references; optional
/// templates are conservatively tried from the rule data and only shown when an actual key
/// resolves.
fn symbol_localisation_preview(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    symbol_name: &str,
    definition: &ResolutionDefinition,
    cancellation: &CancellationToken,
) -> Result<Option<(Option<String>, String)>, Cancelled> {
    if kind.eq_ignore_ascii_case("localisation") {
        return Ok(localisation_preview(snapshot, definition));
    }
    let semantic = &snapshot.rules().model().semantic;
    let has_binding = semantic
        .localisation_bindings
        .iter()
        .any(|binding| binding.type_name.eq_ignore_ascii_case(kind));
    let is_type_definition = semantic
        .type_descriptors
        .keys()
        .any(|type_name| type_name.eq_ignore_ascii_case(kind));
    if !has_binding && !is_type_definition {
        return Ok(None);
    }

    let full_range = definition.location.range;
    let selection_range = definition.selection_range;
    let mut references = Vec::<(String, TextRange)>::new();
    let mut cache_only = false;
    if let Some(document) = definition.location.document.as_ref() {
        if let Some(input) = input_for_document(snapshot, document) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
        }
    } else if let Some(file) = definition.location.file {
        if let Some(input) = input_for_source_file(snapshot, file) {
            references.extend(localisation_references_for_hover(
                snapshot,
                &input,
                cancellation,
            )?);
        } else {
            cache_only = true;
            references.extend(
                snapshot
                    .index()
                    .references(file)
                    .iter()
                    .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
                    .map(|reference| (reference.name.to_string(), reference.range)),
            );
        }
    }
    references.retain(|(_, range)| text_range_within(*range, full_range));
    references.sort_by_key(|(_, range)| {
        (
            if *range == selection_range { 0 } else { 1 },
            range.start(),
            range.end(),
        )
    });
    references.dedup();
    for (name, _) in references {
        cancellation.checkpoint()?;
        for candidate in symbol_candidates_for_hover(snapshot, "localisation", &name, cancellation)?
        {
            if let Some(preview) = localisation_preview(snapshot, &candidate) {
                return Ok(Some(preview));
            }
        }
    }
    if is_type_definition {
        cancellation.checkpoint()?;
        for candidate in
            symbol_candidates_for_hover(snapshot, "localisation", symbol_name, cancellation)?
        {
            if let Some(preview) = localisation_preview(snapshot, &candidate) {
                return Ok(Some(preview));
            }
        }
    }
    if cache_only && !symbol_name.contains('.') {
        for binding in semantic
            .localisation_bindings
            .iter()
            .filter(|binding| binding.type_name.eq_ignore_ascii_case(kind))
        {
            let Some(template) = binding.template.as_deref() else {
                continue;
            };
            let name = template.replace('$', symbol_name);
            cancellation.checkpoint()?;
            for candidate in
                symbol_candidates_for_hover(snapshot, "localisation", &name, cancellation)?
            {
                if let Some(preview) = localisation_preview(snapshot, &candidate) {
                    return Ok(Some(preview));
                }
            }
        }
    }
    Ok(None)
}

fn localisation_references_for_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, TextRange)>, Cancelled> {
    let derived = input
        .hir
        .as_deref()
        .zip(input.path.as_ref())
        .map(|(hir, path)| {
            pdx_engine::hir::derived_localisation_references_for_hover(hir, path, snapshot.rules())
        })
        .unwrap_or_default();
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    let mut references = semantic
        .references
        .into_iter()
        .filter(|reference| reference.kind.eq_ignore_ascii_case("localisation"))
        .map(|reference| (reference.name, reference.range))
        .collect::<Vec<_>>();
    references.extend(
        derived
            .into_iter()
            .map(|reference| (reference.name, reference.range)),
    );
    Ok(references)
}

pub(crate) fn find_cst_node(
    node: CstNode<'_>,
    kind: CstKind,
    range: TextRange,
) -> Option<CstNode<'_>> {
    find_cst_node_bounded(node, kind, range, MAX_CST_SEARCH_DEPTH)
}

/// Nesting bound for CST lookups; localisation files are flat, so this only guards against
/// pathological or corrupted trees, mirroring the bounded-scan rule for workspace scanning.
const MAX_CST_SEARCH_DEPTH: usize = 64;

fn find_cst_node_bounded(
    node: CstNode<'_>,
    kind: CstKind,
    range: TextRange,
    depth: usize,
) -> Option<CstNode<'_>> {
    if node.kind() == kind && node.range() == range {
        return Some(node);
    }
    if depth == 0 {
        return None;
    }
    node.children()
        .find_map(|child| find_cst_node_bounded(child, kind, range, depth - 1))
}

pub(crate) fn truncate_hover_text(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut truncated = String::new();
    let mut overflow = false;
    for (index, character) in value.chars().enumerate() {
        if index == MAX_CHARS {
            overflow = true;
            break;
        }
        truncated.push(character);
    }
    if overflow {
        truncated.push('…');
    }
    truncated
}

/// Renders a symbol spelling as an inline code span without breaking out of the backtick fence.
fn code_span(value: &str) -> String {
    format!("`{}`", value.replace('`', "'"))
}

pub(crate) fn symbol_location_path(location: &Location) -> String {
    location.path.as_ref().map_or_else(
        || "<open document>".to_owned(),
        |path| path.as_str().to_owned(),
    )
}

pub(crate) fn symbol_source_root(snapshot: &AnalysisSnapshot, location: &Location) -> String {
    let root = location
        .file
        .and_then(|file_id| snapshot.source_files().get(&file_id))
        .and_then(|file| {
            snapshot
                .source_roots()
                .iter()
                .find(|root| root.id == file.root_id)
        })
        .or_else(|| {
            location
                .document
                .as_ref()
                .and_then(|document_id| snapshot.document(document_id))
                .and_then(|document| document.path())
                .and_then(|path| root_for_path(snapshot, path))
        });
    match root.map(|root| root.kind) {
        Some(pdx_engine::SourceRootKind::Vanilla) => "Vanilla".to_owned(),
        Some(pdx_engine::SourceRootKind::Dependency) => "Dependency".to_owned(),
        Some(pdx_engine::SourceRootKind::CurrentMod) => "Current Mod".to_owned(),
        None if location.document.is_some() => "Open overlay".to_owned(),
        None => "Unknown source root".to_owned(),
    }
}
pub(crate) fn known_keys(snapshot: &AnalysisSnapshot) -> Arc<BTreeSet<String>> {
    // The key set is a pure function of the immutable snapshot but is consulted on every
    // property-key hover; memoize per revision instead of rebuilding it each time.
    let revision = snapshot.revision();
    let key = "hover-known-keys";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<BTreeSet<String>>(revision, key)
    {
        return cached;
    }
    let mut keys = snapshot
        .game_profile()
        .fallback_keys
        .iter()
        .map(|key| key.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for record in &snapshot.rules().model().records {
        keys.extend(record.fields.keys().map(|key| key.to_ascii_lowercase()));
    }
    // The imported descriptor catalog is the authoritative extension point for semantic keys.
    // Keep profile fallbacks useful in degraded mode, then admit every descriptor name supplied
    // by a validated rules artifact.
    keys.extend(
        snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase()),
    );
    let keys = Arc::new(keys);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        key.to_owned(),
        Arc::clone(&keys),
    );
    keys
}

/// Describes which non-exact first-party matcher family covers a property key. Used by the
/// hover fallback so keys matched through type members, enums, or dates still get provenance
/// instead of silently returning no tooltip. Open-ended matchers (`AnyScalar`, `Dynamic`) are
/// deliberately excluded: they accept every key and would otherwise manufacture tooltips for
/// genuinely unknown properties.
pub(crate) fn semantic_pattern_rule_hint(
    snapshot: &AnalysisSnapshot,
    word: &str,
) -> Option<String> {
    let model = &snapshot.rules().model().semantic;
    let mut families: BTreeSet<&'static str> = BTreeSet::new();
    for rule in &model.rules {
        let family = match &rule.key {
            KeyMatcher::Exact(_) | KeyMatcher::AnyScalar | KeyMatcher::Dynamic(_) => continue,
            KeyMatcher::Type(_) => "a workspace member of its declared type",
            KeyMatcher::Enum(_) => "a member of a first-party enum",
            KeyMatcher::Date => "a campaign date",
        };
        if semantic_key_matches(snapshot, &rule.key, word) {
            families.insert(family);
        }
    }
    if families.is_empty() {
        return None;
    }
    Some(format!(
        "- matched by first-party rules as {}",
        families.into_iter().collect::<Vec<_>>().join(" / ")
    ))
}
