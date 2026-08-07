use std::collections::BTreeSet;

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
        let owner_name = input
            .hir
            .as_deref()
            .and_then(|_| {
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .find(|candidate| candidate.symbol.range == definition.owner_range)
            })
            .map(|definition| definition.name);
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
            hir.parameter_conditionals().iter().any(|conditional| {
                !conditional.negated
                    && conditional.name.eq_ignore_ascii_case(&definition.name)
                    && conditional.range.start() >= definition.owner_range.start()
                    && conditional.range.end() <= definition.owner_range.end()
            })
        });
        let syntax = match reference.kind {
            pdx_engine::hir::HirParameterReferenceKind::Substitution => "substitution",
            pdx_engine::hir::HirParameterReferenceKind::Conditional => "conditional",
        };
        let owner = owner_name.map_or_else(
            || "scripted definition".to_owned(),
            |name| format!("scripted definition `{name}`"),
        );
        return Ok(Some(Hover {
            contents: format!(
                "### parameter `{}`\n\n- Local to {owner}; inferred from its first use\n- Arity: `{}`\n- Syntax: `{syntax}`\n- Occurrences in owner: {occurrences}",
                definition.name,
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
        if !best.contents.contains("#### Localisation preview") {
            for reference in references {
                let hover = hover_for_symbol(
                    snapshot,
                    &reference.kind,
                    &reference.name,
                    range,
                    cancellation,
                )?;
                if hover.contents.contains("#### Localisation preview") {
                    best = hover;
                    break;
                }
            }
        }
        return Ok(Some(best));
    }
    if let Some(definition) = semantic.definitions.iter().find(|definition| {
        definition.document.as_ref() == Some(document)
            && contains(definition.symbol.selection_range, position)
    }) {
        return Ok(Some(hover_for_symbol(
            snapshot,
            &definition.kind,
            &definition.name,
            range,
            cancellation,
        )?));
    }
    cancellation.checkpoint()?;
    if let Some(details) = semantic_rule_hover_at(snapshot, &input, position) {
        return Ok(Some(Hover {
            contents: format!("### PDX property `{word}`\n\n{details}"),
            range: Some(range),
        }));
    }
    if let Some(details) = semantic_value_hover_at(snapshot, &input, position) {
        return Ok(Some(Hover {
            contents: format!("### PDX value `{word}`\n\n{details}"),
            range: Some(range),
        }));
    }
    if is_property_key_at(&input, position) {
        let known = known_keys(snapshot);
        if known.iter().any(|key| key.eq_ignore_ascii_case(&word)) {
            let contents = semantic_rule_documentation(snapshot, &word).map_or_else(
                || format!("### PDX property `{word}`"),
                |details| format!("### PDX property `{word}`\n\n{details}"),
            );
            return Ok(Some(Hover {
                contents,
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
) -> Option<String> {
    let context = semantic_completion_context(snapshot, input, position)?;
    let property = context.property.as_ref()?;
    if !contains(property.key_range, position) {
        return None;
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
    (!candidates.is_empty()).then(|| semantic_rule_hover_for_candidates(snapshot, &candidates))
}

pub(crate) fn semantic_value_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<String> {
    let context = semantic_completion_context(snapshot, input, position)?;
    let property = context.property.as_ref()?;
    let (value, value_range) = property.scalar.as_ref()?;
    if !contains(*value_range, position) {
        return None;
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
        return None;
    }
    let accepted = candidates.iter().any(|candidate| {
        semantic_scope_allows(candidate.rule, candidate.scope)
            && semantic_property_matches(snapshot, candidate.rule, property, candidate.scope)
    });
    Some(format!(
        "- property: `{}`\n- value: `{}`\n- validation: `{}`\n\n{}",
        property.key,
        value,
        if accepted {
            "accepted"
        } else {
            "does not match"
        },
        semantic_rule_hover_for_candidates(snapshot, &candidates)
    ))
}

pub(crate) fn semantic_rule_hover_for_candidates(
    snapshot: &AnalysisSnapshot,
    candidates: &[SemanticCompletionRule<'_, '_>],
) -> String {
    let mut sections = Vec::new();
    if candidates.len() > 1 {
        sections.push(format!(
            "#### {} possible semantic meanings",
            candidates.len()
        ));
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let rule = candidate.rule;
        let title = if candidates.len() > 1 {
            format!("##### Candidate {}", index + 1)
        } else {
            "#### Rule".to_owned()
        };
        let mut details = Vec::new();
        details.push(format!("- context: `{}`", rule.context));
        if !candidate.parent_path.is_empty() {
            details.push(format!("- parent: `{}`", candidate.parent_path.join(".")));
        }
        details.push(format!(
            "- shape: `{}`",
            semantic_rule_shape_label(rule.shape)
        ));
        details.push(format!(
            "- value: `{}`",
            semantic_value_hover_label(&rule.value)
        ));
        if let Some(operator) = rule.operator.as_deref() {
            details.push(format!("- operator: `{operator}`"));
        }
        let child_scope = (rule.push_scope.is_some() || !rule.replace_scope.is_empty())
            .then(|| semantic_child_scope(snapshot, candidate.scope, rule));

        let mut scope_details = Vec::new();
        if !rule.allowed_scopes.is_empty() {
            let allowed = rule
                .allowed_scopes
                .iter()
                .map(|scope| format!("`{scope}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let status = if semantic_scope_allows(rule, candidate.scope) {
                "allowed"
            } else {
                "not allowed"
            };
            scope_details.push(format!("- allowed scopes: {allowed}"));
            scope_details.push(format!(
                "- current scope: `{}` ({status})",
                candidate.scope.current
            ));
        }
        if !rule.allowed_scopes.is_empty() || child_scope.is_some() {
            scope_details.push(format!(
                "- scope registers: {}",
                semantic_scope_register_summary(candidate.scope)
            ));
        }
        if let Some(child_scope) = child_scope.as_ref() {
            scope_details.push(format!(
                "- scope transition: `{}` → `{}`",
                candidate.scope.current, child_scope.current
            ));
            for (register, value) in &rule.replace_scope {
                let resolved = resolve_scope_expression_context(snapshot, candidate.scope, value);
                scope_details.push(format!(
                    "- scope register: `{register}` = `{value}` → `{resolved}`"
                ));
            }
            scope_details.push(format!(
                "- scope registers after: {}",
                semantic_scope_register_summary(child_scope)
            ));
        }

        let mut cardinality_details = Vec::new();
        if rule.required {
            cardinality_details.push("- required".to_owned());
        }
        if let Some(min) = rule.min_occurs {
            cardinality_details.push(format!("- minimum occurrences: {min}"));
        }
        if let Some(max) = rule.max_occurs {
            cardinality_details.push(format!("- maximum occurrences: {max}"));
        }
        if let Some(child_context) = rule.child_context.as_deref() {
            details.push(format!("- child context: `{child_context}`"));
        }
        let mut sections_for_rule = vec![title, details.join("\n")];
        if !scope_details.is_empty() {
            sections_for_rule.push(format!("#### Scope\n\n{}", scope_details.join("\n")));
        }
        if !cardinality_details.is_empty() {
            sections_for_rule.push(format!(
                "#### Cardinality\n\n{}",
                cardinality_details.join("\n")
            ));
        }
        if !rule.source_file.is_empty() && rule.line > 0 {
            sections_for_rule.push(format!(
                "#### Provenance\n\n- rule: `{}:{}`",
                rule.source_file, rule.line
            ));
        }
        if !rule.documentation.is_empty() {
            sections_for_rule.push(format!(
                "#### Documentation\n\n{}",
                rule.documentation.join("  \n")
            ));
        }
        sections.push(sections_for_rule.join("\n\n"));
    }
    sections.join("\n\n")
}

pub(crate) fn semantic_scope_register_summary(scope: &ScopeContext) -> String {
    let mut registers = vec![
        format!("ROOT=`{}`", scope.root),
        format!("THIS=`{}`", scope.current),
    ];
    for (depth, value) in scope.from.iter().enumerate() {
        registers.push(format!("{}=`{value}`", "FROM".repeat(depth + 1)));
    }
    for (depth, value) in scope.previous.iter().enumerate() {
        registers.push(format!("{}=`{value}`", "PREV".repeat(depth + 1)));
    }
    let mut registers = registers.into_iter();
    let Some(first) = registers.next() else {
        return String::new();
    };
    let rest = registers
        .map(|register| format!("  - {register}"))
        .collect::<Vec<_>>();
    if rest.is_empty() {
        first
    } else {
        format!("{first}\n{}", rest.join("\n"))
    }
}

pub(crate) fn semantic_rule_shape_label(shape: RuleShape) -> &'static str {
    match shape {
        RuleShape::Node => "block",
        RuleShape::Leaf => "scalar",
        RuleShape::LeafValue => "bare value",
        RuleShape::ValueClause => "value clause",
    }
}

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
            || rule.min_occurs.is_some()
            || rule.max_occurs != Some(1)
            || !rule.allowed_scopes.is_empty()
    })?;
    semantic_rule_documentation_for_rule(rule)
}

pub(crate) fn semantic_rule_documentation_for_rule(
    rule: &pdx_rules::SemanticRule,
) -> Option<String> {
    let mut sections = Vec::new();
    if !rule.documentation.is_empty() {
        sections.push(format!(
            "#### Documentation\n\n{}",
            rule.documentation.join("  \n")
        ));
    }

    let mut constraints = Vec::new();
    if rule.required {
        constraints.push("- required".to_owned());
    }
    if let Some(min) = rule.min_occurs {
        constraints.push(format!("- minimum occurrences: {min}"));
    }
    if let Some(max) = rule.max_occurs {
        constraints.push(format!("- maximum occurrences: {max}"));
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
    if !rule.source_file.is_empty() && rule.line > 0 {
        sections.push(format!(
            "#### Provenance\n\n- rule: `{}:{}`",
            rule.source_file, rule.line
        ));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}
pub(crate) fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    range: TextRange,
    cancellation: &CancellationToken,
) -> Result<Hover, Cancelled> {
    let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
    let policy = symbol_resolution_policy(snapshot, kind);
    let mut sections = vec![format!("### {} `{}`", kind, name)];
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
            if kind.eq_ignore_ascii_case("localisation")
                && let Some((language, value)) = localisation_preview(snapshot, definition)
            {
                sections.push(format!(
                    "#### Localisation preview\n\n- Localisation{}: \"{}\"",
                    language
                        .as_deref()
                        .map_or_else(String::new, |language| format!(" ({language})")),
                    value
                ));
            }
        } else {
            sections.push(format!("#### ambiguous {kind} symbol"));
            sections.push(format!(
                "#### Candidates:\n\n{}",
                candidates
                    .iter()
                    .map(|candidate| format!(
                        "- {}: `{}`",
                        symbol_source_root(snapshot, &candidate.location),
                        symbol_location_path(&candidate.location),
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    Ok(Hover {
        contents: sections.join("\n\n"),
        range: Some(range),
    })
}

pub(crate) fn localisation_preview(
    snapshot: &AnalysisSnapshot,
    definition: &ResolutionDefinition,
) -> Option<(Option<String>, String)> {
    if let Some(file) = definition.location.file
        && let Some(preview) =
            snapshot.vanilla_localisation_preview(file, definition.location.range)
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
    let value_node = entry.children().iter().find(|child| {
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
                .iter()
                .find(|child| child.kind() == CstKind::LocalisationKey)
                .and_then(|child| parsed.text(child.range()))
        {
            language = Some(value.trim().to_owned());
        }
    }
    Some((language, value))
}

pub(crate) fn find_cst_node(node: &CstNode, kind: CstKind, range: TextRange) -> Option<&CstNode> {
    if node.kind() == kind && node.range() == range {
        return Some(node);
    }
    node.children()
        .iter()
        .find_map(|child| find_cst_node(child, kind, range))
}

pub(crate) fn truncate_hover_text(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut truncated = value.chars().take(MAX_CHARS).collect::<String>();
    if value.chars().count() > MAX_CHARS {
        truncated.push('…');
    }
    truncated
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
pub(crate) fn known_keys(snapshot: &AnalysisSnapshot) -> BTreeSet<String> {
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
    keys
}
