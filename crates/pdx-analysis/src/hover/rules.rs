//! Semantic-rule hovers: property keys, scalar values, documentation, and provenance hints.

use std::collections::BTreeSet;

use super::render::{HoverModel, code_span};
use crate::completion::{
    SemanticCompletionRule, semantic_completion_context_with_cancellation,
    semantic_rules_for_completion,
};
use crate::messages::numeric_bounds;
use crate::semantic::{
    semantic_child_scope, semantic_key_matches, semantic_property_matches,
    semantic_rule_key_matches, semantic_scope_allows,
};
use crate::support::{ParsedInput, contains, truncate_hover_text};
use crate::types::{CancellationToken, Cancelled};
use pdx_engine::AnalysisSnapshot;
use pdx_rules::{KeyMatcher, RuleShape, ValueMatcher};
use pdx_text::TextSize;

pub(crate) fn semantic_rule_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
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
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut model = HoverModel::new(semantic_rule_hover_title(word, &candidates, ""));
    model.extend_sections(semantic_rule_hover_for_candidates(
        snapshot,
        word,
        &candidates,
    ));
    Ok(Some(model))
}

pub(crate) fn semantic_value_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
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
    let mut model = HoverModel::new(semantic_rule_hover_title(word, &candidates, " value"));
    model.push_section(format!(
        "- property: `{}`\n- value: `{}`\n- validation: `{}`",
        property.key,
        value,
        if accepted {
            "accepted"
        } else {
            "does not match"
        },
    ));
    model.extend_sections(semantic_rule_hover_for_candidates(
        snapshot,
        word,
        &candidates,
    ));
    Ok(Some(model))
}

/// Hover title in the symbol-hover pattern: the rule-context category (or
/// nothing when candidates disagree or carry no context). `suffix` marks
/// value-position hovers (`### Effect value \`my_flag\``).
fn semantic_rule_hover_title(
    word: &str,
    candidates: &[SemanticCompletionRule<'_, '_>],
    suffix: &str,
) -> String {
    let Some(shared) = shared_rule_context(candidates) else {
        return format!("### {}", code_span(word));
    };
    format!(
        "### {}{suffix} {}",
        semantic_context_category(&shared),
        code_span(word)
    )
}

/// The one context every candidate agrees on, when they agree.
fn shared_rule_context(candidates: &[SemanticCompletionRule<'_, '_>]) -> Option<String> {
    let first = candidates.first()?.rule.context.clone();
    candidates
        .iter()
        .all(|candidate| candidate.rule.context.eq_ignore_ascii_case(&first))
        .then_some(first)
}

/// Display category for a rule context: the three command namespaces keep
/// their canonical names; everything else humanizes the context identifier
/// (compound `root:` contexts use their tail segment). Callers that cannot
/// establish a category fall back to the bare symbol-hover title.
pub(crate) fn semantic_context_category(context: &str) -> String {
    let context = context.strip_prefix("root:").unwrap_or(context);
    match context {
        "trigger" => "Trigger".to_owned(),
        "effect" => "Effect".to_owned(),
        "modifier" => "Modifier".to_owned(),
        other => {
            let mut name = other.replace('_', " ");
            let mut characters = name.chars();
            match characters.next() {
                Some(first) => {
                    name = first.to_uppercase().collect::<String>() + characters.as_str();
                    name
                }
                None => name,
            }
        }
    }
}

/// Renders the hover sections for every distinct meaning of the matched candidates.
///
/// Sections are returned unrendered so callers embed them into their own hover model; the
/// candidate slice is never empty at the call sites (both hover entry points gate on it).
///
/// Candidates split into value types (rendered under `#### Allowed value types`,
/// renamed from "Possible meanings" because the entries describe accepted value
/// shapes) and scope links (block keys that re-target the scope — not value
/// types), whose transitions render in the ambient `#### Scope` table instead.
pub(crate) fn semantic_rule_hover_for_candidates(
    snapshot: &AnalysisSnapshot,
    word: &str,
    candidates: &[SemanticCompletionRule<'_, '_>],
) -> Vec<String> {
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
    let (value_typed, scope_links): (Vec<_>, Vec<_>) = candidates
        .iter()
        .partition(|candidate| !is_scope_link_rule(candidate.rule));

    let mut sections = Vec::new();
    if value_typed.len() > 1 {
        let shared_documentation = shared_semantic_hover_documentation(&value_typed);
        let summaries = value_typed
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
        sections.push(format!(
            "#### Allowed value types ({})\n\n{summaries}",
            value_typed.len()
        ));
        if let Some(documentation) = shared_documentation {
            sections.push(format!(
                "#### Documentation\n\n{}",
                truncate_documentation(&documentation)
            ));
        }
    } else if let Some(candidate) = value_typed.first() {
        sections.push(semantic_hover_candidate_details(snapshot, candidate).join("\n"));
        let cardinality = semantic_hover_cardinality_details(candidate.rule);
        if !cardinality.is_empty() {
            sections.push(format!("#### Constraints\n\n{}", cardinality.join("\n")));
        }
        if !candidate.rule.documentation.is_empty() {
            sections.push(format!(
                "#### Documentation\n\n{}",
                truncate_documentation(&candidate.rule.documentation)
            ));
        }
    } else if let Some(candidate) = scope_links.first() {
        // A pure scope link has no value shape to describe; its scope
        // requirements stay visible as details and its transition renders in
        // the Scope table below.
        let details = semantic_hover_scope_link_details(candidate);
        if !details.is_empty() {
            sections.push(details.join("\n"));
        }
        let cardinality = semantic_hover_cardinality_details(candidate.rule);
        if !cardinality.is_empty() {
            sections.push(format!("#### Constraints\n\n{}", cardinality.join("\n")));
        }
        if !candidate.rule.documentation.is_empty() {
            sections.push(format!(
                "#### Documentation\n\n{}",
                truncate_documentation(&candidate.rule.documentation)
            ));
        }
    }
    if let Some(scope) = ambient_scope_section(snapshot, word, candidates, &scope_links) {
        sections.push(scope);
    }
    sections
}

/// A block rule that re-targets the scope (`owner = { … }`): its hover entry
/// is a scope transition, not a value type.
fn is_scope_link_rule(rule: &pdx_rules::SemanticRule) -> bool {
    matches!(rule.shape, RuleShape::Node)
        && (rule.push_scope.is_some() || !rule.replace_scope.is_empty())
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
        details.push(semantic_hover_scope_line(candidate));
    }
    if (rule.push_scope.is_some() || !rule.replace_scope.is_empty()) && {
        let child_scope = semantic_child_scope(snapshot, candidate.scope, rule);
        !candidate
            .scope
            .current
            .eq_ignore_ascii_case(&child_scope.current)
    } {
        // Non-link blocks can still carry a scope transition (transparent
        // wrappers re-target registers); value-shaped candidates keep the
        // inline line because the Scope table only lists scope links.
        let child_scope = semantic_child_scope(snapshot, candidate.scope, rule);
        details.push(format!(
            "- scope transition: `{}` → `{}`",
            candidate.scope.current, child_scope.current
        ));
    }
    details
}

/// The `valid scopes` line, or the unavailable variant when the ambient
/// scope is not among them.
fn semantic_hover_scope_line(candidate: &SemanticCompletionRule<'_, '_>) -> String {
    let allowed = candidate
        .rule
        .allowed_scopes
        .iter()
        .map(|scope| format!("`{scope}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if semantic_scope_allows(candidate.rule, candidate.scope) {
        format!("- valid scopes: {allowed}")
    } else {
        format!(
            "- unavailable in current scope `{}`; valid scopes: {allowed}",
            candidate.scope.current
        )
    }
}

fn semantic_hover_scope_link_details(candidate: &SemanticCompletionRule<'_, '_>) -> Vec<String> {
    candidate
        .rule
        .allowed_scopes
        .is_empty()
        .then(Vec::new)
        .unwrap_or_else(|| vec![semantic_hover_scope_line(candidate)])
}

/// The ambient scope table: where the hovered statement sits (current scope
/// and the root/prev/from registers) and where each scope-link candidate
/// re-targets. Skipped entirely when nothing beyond an unknown scope is
/// known, so degraded contexts do not grow noise lines.
fn ambient_scope_section(
    snapshot: &AnalysisSnapshot,
    word: &str,
    candidates: &[&SemanticCompletionRule<'_, '_>],
    scope_links: &[&SemanticCompletionRule<'_, '_>],
) -> Option<String> {
    let scope = &candidates.first()?.scope;
    // `any` registers are unknown placeholders (an untracked root, an unused
    // FROM); rendering them would only add noise lines.
    let known = |value: &str| !value.eq_ignore_ascii_case("any");
    let mut lines = vec![format!("- here: `{}`", scope.current)];
    if known(&scope.root) && !scope.root.eq_ignore_ascii_case(&scope.current) {
        lines.push(format!("- root: `{}`", scope.root));
    }
    if let Some(previous) = scope.previous.last().filter(|value| known(value)) {
        lines.push(format!("- prev: `{previous}`"));
    }
    if let Some(from) = scope.from.last().filter(|value| known(value)) {
        lines.push(format!("- from: `{from}`"));
    }
    for candidate in scope_links {
        let child_scope = semantic_child_scope(snapshot, candidate.scope, candidate.rule);
        let mut line = format!("- `{}` enters `{}`", word, child_scope.current);
        if !candidate.rule.allowed_scopes.is_empty() {
            let allowed = candidate
                .rule
                .allowed_scopes
                .iter()
                .map(|scope| format!("`{scope}`"))
                .collect::<Vec<_>>()
                .join(", ");
            line.push_str(&format!(" (from {allowed})"));
        }
        lines.push(line);
    }
    let ambient_known = known(&scope.current)
        || scope.previous.iter().any(|value| known(value))
        || scope.from.iter().any(|value| known(value));
    (ambient_known || !scope_links.is_empty())
        .then(|| format!("#### Scope\n\n{}", lines.join("\n")))
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
    // Rule-level equivalence ignores declaration provenance (id, alternative_id, source_file,
    // line); see `SemanticRule::semantic_equivalent`.
    left.parent_path == right.parent_path
        && left.scope == right.scope
        && left.rule.semantic_equivalent(right.rule)
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
            semantic_numeric_hover_label("float", min.as_deref(), max.as_deref())
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
        ValueMatcher::TypedPrefix {
            prefix, context, ..
        } => {
            format!("`{prefix}` + numeric or bool `{context}`")
        }
        ValueMatcher::Opaque(value) => format!("opaque `{value}`"),
    }
}

pub(crate) fn semantic_numeric_hover_label<T: std::fmt::Display>(
    kind: &str,
    min: Option<T>,
    max: Option<T>,
) -> String {
    format!("{kind}{}", numeric_bounds(min, max).label_suffix())
}

pub(crate) fn semantic_rule_documentation(
    snapshot: &AnalysisSnapshot,
    key: &str,
) -> Option<String> {
    // The lookup is a pure function of the immutable rules model but scans every semantic rule
    // per known-key hover; memoize per revision. The matcher is case-insensitive, so the cache
    // key is normalized rather than verbatim.
    let revision = snapshot.revision();
    let cache_key = format!("hover-rule-documentation:{}", key.to_ascii_lowercase());
    if let Some(cached) = snapshot
        .query_cache()
        .get::<Option<String>>(revision, &cache_key)
    {
        return (*cached).clone();
    }
    let documentation = semantic_rule_documentation_uncached(snapshot, key);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        cache_key,
        std::sync::Arc::new(documentation.clone()),
    );
    documentation
}

fn semantic_rule_documentation_uncached(snapshot: &AnalysisSnapshot, key: &str) -> Option<String> {
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

/// The display category for a known key's rule family, when its exact-key
/// rules agree on one context. Memoized per revision alongside the
/// documentation lookup because it scans the same rule list.
pub(crate) fn semantic_rule_key_category(snapshot: &AnalysisSnapshot, key: &str) -> Option<String> {
    let revision = snapshot.revision();
    let cache_key = format!("hover-rule-category:{}", key.to_ascii_lowercase());
    if let Some(cached) = snapshot
        .query_cache()
        .get::<Option<String>>(revision, &cache_key)
    {
        return (*cached).clone();
    }
    let mut contexts = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .filter(|rule| match &rule.key {
            KeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
            _ => false,
        })
        .map(|rule| rule.context.clone())
        .collect::<Vec<_>>();
    contexts.sort();
    contexts.dedup();
    let category = (contexts.len() == 1)
        .then(|| {
            contexts
                .first()
                .map(|context| semantic_context_category(context))
        })
        .flatten();
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        cache_key,
        std::sync::Arc::new(category.clone()),
    );
    category
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
