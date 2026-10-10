//! IDE queries backed directly by the lowered Rules IR.
//!
//! HIR owns the source-to-schema walk. This module only interprets its cached
//! facts and does not rebuild semantic context from paths or legacy rules.

use crate::template_parse::{TemplateParse, TemplateParseSession};
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode, Severity};
use crate::{semantic::effective_workspace_member_names, support::ParsedInput};
use engine::AnalysisSnapshot;
use hir::{HirFile, ScopeState, ScopeValue};
use rules::ir::{
    FieldId, FieldValue, Matcher, MatcherId, PatternPart, RefTarget, RulesIr, SchemaId, Shape,
    SymbolFacts, TypeId,
};
use rules::source::{ControlKind, Severity as RuleSeverity};
use std::collections::{BTreeMap, BTreeSet};
use text::TextRange;

/// Workspace-backed symbol lookup for the IR's type and trait matchers.
pub(crate) type WorkspaceFacts<'a> = crate::ir_queries::SnapshotSymbolFacts<'a>;

/// Returns true when the selected document has an IR schema path with cached HIR facts.
pub(crate) fn has_ir_schema(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> bool {
    let (Some(path), Some(hir)) = (input.path.as_ref(), input.hir.as_deref()) else {
        return false;
    };
    if !hir.uses_ir() || snapshot.ir().schemas.is_empty() {
        return false;
    }
    snapshot
        .ir()
        .file_rule(path)
        .is_some_and(|(_, rule)| rule.parser == rules::ir::DocumentParser::Script)
}

pub(crate) fn matcher_matches_in_snapshot(
    snapshot: &AnalysisSnapshot,
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
) -> bool {
    let state = ScopeState::initial(ScopeValue::Unknown);
    matcher_matches_with_state(snapshot, ir, matcher, value, facts, &state)
}

pub(crate) fn matcher_matches_with_state(
    _snapshot: &AnalysisSnapshot,
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
    state: &ScopeState,
) -> bool {
    hir::checking::scalar_validation(ir, matcher, value, state, facts)
        != hir::analysis::Validation::Invalid
}

/// Validates a scalar against its physical sibling container.
pub(crate) fn matcher_matches_with_context(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
    state: &ScopeState,
    context: &impl rules::query::QueryContext,
) -> bool {
    hir::checking::scalar_validation_with_context(ir, matcher, value, state, facts, context)
        != hir::analysis::Validation::Invalid
}

/// Human-readable description of a complete IR matcher.
pub(crate) fn describe(ir: &RulesIr, matcher: MatcherId) -> String {
    match ir.matcher(matcher) {
        Matcher::Scalar => "any scalar".into(),
        Matcher::Literal(value) => format!("`{}`", ir.strings().resolve(*value)),
        Matcher::Pattern(parts) => format!("template `{}`", pattern_text(ir, parts)),
        Matcher::Int { min, max } => describe_range("integer", *min, *max),
        Matcher::Float { min, max } => describe_range("number", *min, *max),
        Matcher::Bool => "yes or no".into(),
        Matcher::Date => "a campaign date".into(),
        Matcher::Loc => "a localisation key".into(),
        Matcher::Path(None) => "a file path".into(),
        Matcher::Path(Some(category)) => {
            format!("a {} file path", ir.strings().resolve(*category))
        }
        Matcher::Ref(RefTarget::Type {
            type_id, subtype, ..
        }) => describe_type(ir, *type_id, *subtype),

        Matcher::Def { type_id, subtype } => {
            format!("a definition of {}", describe_type(ir, *type_id, *subtype))
        }
        Matcher::Enum { id } => {
            let info = ir.enum_info(*id);
            let values = info
                .rows
                .iter()
                .map(|row| ir.strings().resolve(row.spelling))
                .collect::<Vec<_>>();
            if values.is_empty() {
                format!("a `{}` value", ir.strings().resolve(info.name))
            } else {
                format!("one of {}", quoted_list(&values))
            }
        }
        Matcher::Scope(None) => "a scope or scope link".into(),
        Matcher::Scope(Some(scope)) => format!("a `{}` scope", ir.strings().resolve(*scope)),
        Matcher::Link => "a scope link or register".into(),
        Matcher::Opaque => "text".into(),
        Matcher::Query(query) => format!(
            "{} of `{}`",
            match query.projection {
                rules::ir::QueryProjection::Keys => "field keys",
                rules::ir::QueryProjection::Values => "selected field values",
            },
            ir.strings().resolve(ir.schema(query.schema).name),
        ),
        Matcher::Union(alternatives) => {
            let mut descriptions = alternatives
                .iter()
                .map(|id| describe(ir, *id))
                .collect::<Vec<_>>();
            descriptions.dedup();
            descriptions.join(" or ")
        }
    }
}

/// Candidate spellings accepted by a matcher, using the current workspace index for refs.
pub(crate) fn spellings(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    spellings_into(
        ir,
        matcher,
        snapshot,
        prefix,
        &rules::query::NoQueryContext,
        &mut out,
    );
    out.sort_by_key(|value| value.to_ascii_lowercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

/// Enumerates query projections without erasing their source field identities.
pub(crate) fn projected_matchers(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    context: &impl rules::query::QueryContext,
) -> Vec<(MatcherId, Option<FieldId>)> {
    fn collect(
        ir: &RulesIr,
        matcher: MatcherId,
        facts: &impl SymbolFacts,
        context: &impl rules::query::QueryContext,
        origin: Option<FieldId>,
        depth: usize,
        out: &mut Vec<(MatcherId, Option<FieldId>)>,
    ) {
        if depth >= 64 {
            return;
        }
        match ir.matcher(matcher) {
            Matcher::Query(query) => {
                let resolved = ir.query_fields(query, facts, context);
                if resolved.state != rules::query::QueryState::Resolved {
                    return;
                }
                for field in resolved.fields {
                    let projected = match query.projection {
                        rules::ir::QueryProjection::Keys => Some(ir.field(field).key),
                        rules::ir::QueryProjection::Values => match ir.field(field).value {
                            FieldValue::Scalar(value) => Some(value),
                            _ => None,
                        },
                    };
                    if let Some(projected) = projected {
                        collect(ir, projected, facts, context, Some(field), depth + 1, out);
                    }
                }
            }
            Matcher::Union(alternatives) => {
                for alternative in alternatives {
                    collect(ir, *alternative, facts, context, origin, depth + 1, out);
                }
            }
            _ => out.push((matcher, origin)),
        }
    }
    let mut out = Vec::new();
    collect(
        ir,
        matcher,
        &WorkspaceFacts { snapshot },
        context,
        None,
        0,
        &mut out,
    );
    out
}

/// Whether matcher interpretation needs query context, including Pattern holes.
pub(crate) fn has_field_query(ir: &RulesIr, matcher: MatcherId) -> bool {
    match ir.matcher(matcher) {
        Matcher::Query(_) => true,
        Matcher::Union(items) => items.iter().any(|id| has_field_query(ir, *id)),
        Matcher::Pattern(parts) => parts
            .iter()
            .any(|part| matches!(part, PatternPart::Hole(id) if has_field_query(ir, *id))),
        _ => false,
    }
}

fn has_value_query(ir: &RulesIr, matcher: MatcherId) -> bool {
    match ir.matcher(matcher) {
        Matcher::Query(query) => query.projection == rules::ir::QueryProjection::Values,
        Matcher::Union(items) => items.iter().any(|id| has_value_query(ir, *id)),
        Matcher::Pattern(parts) => parts
            .iter()
            .any(|part| matches!(part, PatternPart::Hole(id) if has_value_query(ir, *id))),
        _ => false,
    }
}

/// Returns the original declaration supplying a concrete projected spelling.
pub(crate) fn projected_source(
    ir: &RulesIr,
    matcher: MatcherId,
    spelling: &str,
    snapshot: &AnalysisSnapshot,
    context: &impl rules::query::QueryContext,
) -> Option<FieldId> {
    if !has_field_query(ir, matcher) {
        return None;
    }
    fn source(
        ir: &RulesIr,
        matcher: MatcherId,
        spelling: &str,
        snapshot: &AnalysisSnapshot,
        context: &impl rules::query::QueryContext,
        depth: usize,
    ) -> Option<FieldId> {
        if depth >= 64 {
            return None;
        }
        match ir.matcher(matcher) {
            Matcher::Pattern(parts) => {
                let facts = WorkspaceFacts { snapshot };
                let mut no_cancel = || false;
                let mut budget =
                    rules::pattern::SearchBudget::new(Default::default(), &mut no_cancel);
                let query_context = rules::query::QueryContextWithFacts::new(ir, &facts, context);
                let result = rules::pattern::unique_search_with_context(
                    ir,
                    parts,
                    spelling,
                    &mut budget,
                    &query_context,
                    &mut |id, text| {
                        hir::checking::scalar_outcome_with_context(ir, id, text, &facts, context).0
                    },
                );
                result.holes.into_iter().find_map(|(id, start, end)| {
                    spelling
                        .get(start..end)
                        .and_then(|value| source(ir, id, value, snapshot, context, depth + 1))
                })
            }
            Matcher::Union(items) => items
                .iter()
                .find_map(|id| source(ir, *id, spelling, snapshot, context, depth + 1)),
            Matcher::Query(query) => {
                let facts = WorkspaceFacts { snapshot };
                let resolved = if query.projection == rules::ir::QueryProjection::Keys
                    && query.selector.is_none()
                {
                    ir.query_selected_fields(query, spelling, &facts, context)
                } else {
                    ir.query_fields(query, &facts, context)
                };
                resolved.fields.into_iter().find(|field| {
                    ir.query_projected_matcher(query, *field)
                        .is_some_and(|projected| {
                            hir::checking::scalar_outcome_with_context(
                                ir, projected, spelling, &facts, context,
                            )
                            .0 == Some(true)
                        })
                })
            }
            _ => None,
        }
    }
    // Identity and provenance survive a rejected empty-argument invocation. This
    // context relaxes only call eligibility, never dispatch or source membership.
    let context = rules::query::ReferenceQueryContext::new(context);
    source(ir, matcher, spelling, snapshot, &context, 0)
}

/// Compatibility controls and authored query matchers share the same projection.
pub(crate) fn query_spellings(
    ir: &RulesIr,
    query: &rules::ir::FieldQuery,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    state: Option<&ScopeState>,
    context: &impl rules::query::QueryContext,
) -> Vec<(String, FieldId)> {
    let facts = WorkspaceFacts { snapshot };
    let resolution = ir.query_fields(query, &facts, context);
    if resolution.state != rules::query::QueryState::Resolved {
        return Vec::new();
    }
    let mut out = Vec::new();
    for field in resolution.fields {
        if let Some(matcher) = ir.query_projected_matcher(query, field) {
            out.extend(
                spellings_with_context(ir, matcher, snapshot, prefix, state, context)
                    .into_iter()
                    .filter(|value| {
                        let outcome = ir.query_outcome(query, value, &facts, context);
                        if query.call_args_none {
                            outcome == Some(true)
                        } else {
                            outcome != Some(false)
                        }
                    })
                    .map(|value| (value, field)),
            );
        }
    }
    out
}

pub(crate) fn spellings_with_context(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    state: Option<&ScopeState>,
    context: &impl rules::query::QueryContext,
) -> Vec<String> {
    if let Matcher::Query(query) = ir.matcher(matcher) {
        let mut values = projected_matchers(ir, matcher, snapshot, context)
            .into_iter()
            .flat_map(|(projected, _)| {
                spellings_with_context(ir, projected, snapshot, prefix, state, context)
            })
            .filter(|value| {
                let outcome = hir::checking::scalar_outcome_with_context(
                    ir,
                    matcher,
                    value,
                    &WorkspaceFacts { snapshot },
                    context,
                )
                .0;
                if query.call_args_none {
                    outcome == Some(true)
                } else {
                    outcome != Some(false)
                }
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|value| value.to_ascii_lowercase());
        values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        return values;
    }
    if let Matcher::Union(alternatives) = ir.matcher(matcher) {
        let mut values = alternatives
            .iter()
            .flat_map(|alternative| {
                spellings_with_context(ir, *alternative, snapshot, prefix, state, context)
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|value| value.to_ascii_lowercase());
        values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        return values;
    }
    let mut values = Vec::new();
    spellings_into(ir, matcher, snapshot, prefix, context, &mut values);
    values.sort_by_key(|value| value.to_ascii_lowercase());
    values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    if matches!(ir.matcher(matcher), Matcher::Scope(_) | Matcher::Link) {
        // Keep the default list bounded to pairs of static declared links. Dynamic
        // link operands are workspace-sized and must not form a Cartesian product.
        let links = ir
            .scopes
            .links
            .iter()
            .filter(|link| {
                link.pattern
                    .iter()
                    .all(|part| matches!(part, PatternPart::Text(_)))
            })
            .map(|link| pattern_text(ir, &link.pattern))
            .collect::<Vec<_>>();
        let expected = match ir.matcher(matcher) {
            Matcher::Scope(expected) => *expected,
            _ => None,
        };
        for first in &links {
            for second in &links {
                let name = format!("{first}.{second}");
                if name
                    .to_ascii_lowercase()
                    .contains(&prefix.to_ascii_lowercase())
                    && scope_expression_allowed(ir, snapshot, &name, expected, state)
                {
                    values.push(name);
                }
            }
        }
        values.sort_by_key(|value| value.to_ascii_lowercase());
        values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    }
    if let Matcher::Scope(expected) = ir.matcher(matcher) {
        values.retain(|name| scope_expression_allowed(ir, snapshot, name, *expected, state));
    } else if matches!(ir.matcher(matcher), Matcher::Link) {
        values.retain(|name| scope_expression_allowed(ir, snapshot, name, None, state));
    }
    values
}

fn scope_expression_allowed(
    ir: &RulesIr,
    snapshot: &AnalysisSnapshot,
    name: &str,
    expected: Option<rules::ir::Symbol>,
    state: Option<&ScopeState>,
) -> bool {
    hir::checking::scope_expression_allowed(ir, name, expected, state, &WorkspaceFacts { snapshot })
}

fn spellings_into(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    context: &impl rules::query::QueryContext,
    out: &mut Vec<String>,
) {
    match ir.matcher(matcher) {
        Matcher::Literal(value) => out.push(ir.strings().resolve(*value).to_owned()),
        Matcher::Pattern(parts) => pattern_spellings(ir, parts, snapshot, prefix, context, out),
        Matcher::Bool => out.extend(["yes".into(), "no".into()]),
        Matcher::Date => {}
        Matcher::Enum { id } => {
            let info = ir.enum_info(*id);
            out.extend(
                info.rows
                    .iter()
                    .map(|row| ir.strings().resolve(row.spelling).to_owned()),
            );
        }
        Matcher::Scope(scope) => {
            out.extend(
                ir.scopes
                    .types
                    .iter()
                    .copied()
                    .filter(|name| {
                        scope.is_none_or(|expected| ir.scopes_compatible(*name, expected))
                    })
                    .map(|name| ir.strings().resolve(name).to_owned()),
            );
            out.extend(
                scope_link_and_register_names(ir, snapshot)
                    .into_iter()
                    .filter(|name| {
                        ir.scopes.register(ir.strings(), name).is_some()
                            || scope.is_none_or(|expected| {
                                ir.scopes.links.iter().any(|link| {
                                    link.to.type_name().is_some_and(|actual| {
                                        ir.scopes_compatible(actual, expected)
                                    }) && scope_link_spellings(ir, link, snapshot)
                                        .iter()
                                        .any(|candidate| candidate.eq_ignore_ascii_case(name))
                                }) || name.eq_ignore_ascii_case(ir.strings().resolve(expected))
                            })
                    }),
            );
        }
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            let facts = WorkspaceFacts { snapshot };
            let name = ir.strings().resolve(ir.type_info(*type_id).name);
            let strip = strip_prefix.map(|prefix| ir.strings().resolve(prefix));
            if subtype.is_none() {
                out.extend(
                    ir.type_info(*type_id)
                        .builtin
                        .iter()
                        .map(|member| ir.strings().resolve(*member).to_owned()),
                );
            }
            out.extend(
                effective_workspace_member_names(snapshot, name)
                    .into_iter()
                    .filter(|member| {
                        let subtype_ok = subtype.is_none_or(|subtype| {
                            facts.type_subtype_member(*type_id, subtype, member)
                        });
                        subtype_ok
                            && strip.is_none_or(|strip| {
                                strip_prefix_case_insensitive(member, strip).is_some()
                            })
                    })
                    .map(|member| {
                        strip.map_or(member.clone(), |strip| {
                            strip_prefix_case_insensitive(&member, strip)
                                .unwrap_or(&member)
                                .to_owned()
                        })
                    }),
            );
        }

        Matcher::Def { type_id, .. } => {
            let name = ir.strings().resolve(ir.type_info(*type_id).name);
            out.extend(effective_workspace_member_names(snapshot, name));
            out.extend(
                ir.type_info(*type_id)
                    .builtin
                    .iter()
                    .map(|member| ir.strings().resolve(*member).to_owned()),
            );
        }
        Matcher::Loc => out.extend(effective_workspace_member_names(snapshot, "localisation")),
        Matcher::Union(items) => {
            for item in items {
                spellings_into(ir, *item, snapshot, prefix, context, out);
            }
        }
        Matcher::Query(_) => out.extend(spellings_with_context(
            ir, matcher, snapshot, prefix, None, context,
        )),
        Matcher::Scalar
        | Matcher::Int { .. }
        | Matcher::Float { .. }
        | Matcher::Path(_)
        | Matcher::Opaque => {}
        Matcher::Link => out.extend(scope_link_and_register_names(ir, snapshot)),
    }
}

fn pattern_spellings(
    ir: &RulesIr,
    parts: &[PatternPart],
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    context: &impl rules::query::QueryContext,
    out: &mut Vec<String>,
) {
    let mut variants = vec![String::new()];
    for part in parts {
        let options = match part {
            PatternPart::Text(text) => vec![ir.strings().resolve(*text).to_owned()],
            PatternPart::Hole(matcher) => {
                let mut values = Vec::new();
                spellings_into(ir, *matcher, snapshot, "", context, &mut values);
                values.sort_by_key(|value| value.to_ascii_lowercase());
                values.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                values
            }
        };
        if options.is_empty() {
            return;
        }
        let mut next = Vec::new();
        'outer: for variant in &variants {
            for option in &options {
                if next.len() >= 2048 {
                    break 'outer;
                }
                next.push(format!("{variant}{option}"));
            }
        }
        variants = next;
    }
    out.extend(variants.into_iter().filter(|value| {
        value
            .to_ascii_lowercase()
            .contains(&prefix.to_ascii_lowercase())
    }));
}

fn scope_link_and_register_names(ir: &RulesIr, snapshot: &AnalysisSnapshot) -> Vec<String> {
    let mut names = Vec::new();
    for link in ir.scopes.links.iter() {
        names.extend(scope_link_spellings(ir, link, snapshot));
    }
    for register in ir.scopes.registers.iter() {
        let name = ir.strings().resolve(register.name).to_ascii_uppercase();
        names.push(name.clone());
        if register.chain {
            for depth in 2..=4 {
                names.push(name.repeat(depth));
                names.push(
                    std::iter::repeat_n(name.as_str(), depth)
                        .collect::<Vec<_>>()
                        .join("_"),
                );
            }
        }
    }
    names
}

fn scope_link_spellings(
    ir: &RulesIr,
    link: &rules::ir::LinkInfo,
    snapshot: &AnalysisSnapshot,
) -> Vec<String> {
    let mut names = vec![String::new()];
    for part in link.pattern.iter() {
        let options = match part {
            PatternPart::Text(text) => vec![ir.strings().resolve(*text).to_owned()],
            PatternPart::Hole(matcher) => {
                let mut options = Vec::new();
                match ir.matcher(*matcher) {
                    Matcher::Scope(scope) => options.extend(
                        ir.scopes
                            .types
                            .iter()
                            .filter(|name| scope.is_none_or(|scope| scope == **name))
                            .map(|name| ir.strings().resolve(*name).to_owned()),
                    ),
                    Matcher::Link => options.extend(
                        ir.scopes
                            .registers
                            .iter()
                            .map(|register| ir.strings().resolve(register.name).to_owned()),
                    ),
                    _ => spellings_into(
                        ir,
                        *matcher,
                        snapshot,
                        "",
                        &rules::query::NoQueryContext,
                        &mut options,
                    ),
                }
                options.sort_by_key(|value| value.to_ascii_lowercase());
                options.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                options
            }
        };
        if options.is_empty() {
            return Vec::new();
        }
        let mut next = Vec::new();
        'outer: for prefix in &names {
            for option in &options {
                if next.len() >= 2048 {
                    break 'outer;
                }
                next.push(format!("{prefix}{option}"));
            }
        }
        names = next;
    }
    names
}

fn strip_prefix_case_insensitive<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .and_then(|_| value.get(prefix.len()..))
}

fn describe_type(ir: &RulesIr, type_id: TypeId, subtype: Option<rules::ir::Symbol>) -> String {
    let name = ir.strings().resolve(ir.type_info(type_id).name);
    subtype.map_or_else(
        || format!("a `{name}` instance"),
        |subtype| format!("a `{}` `{name}` instance", ir.strings().resolve(subtype)),
    )
}

fn describe_range<T: std::fmt::Display>(name: &str, min: Option<T>, max: Option<T>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("a {name} from {min} to {max}"),
        (Some(min), None) => format!("a {name} of at least {min}"),
        (None, Some(max)) => format!("a {name} of at most {max}"),
        (None, None) => format!("a {name}"),
    }
}

fn pattern_text(ir: &RulesIr, parts: &[PatternPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            PatternPart::Text(text) => ir.strings().resolve(*text).to_owned(),
            PatternPart::Hole(_) => "{…}".into(),
        })
        .collect()
}

fn quoted_list(values: &[&str]) -> String {
    match values {
        [] => String::new(),
        [one] => format!("`{one}`"),
        [one, two] => format!("`{one}` or `{two}`"),
        many => format!(
            "{} or `{}`",
            many[..many.len() - 1]
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", "),
            many[many.len() - 1]
        ),
    }
}

/// Runs schema-driven diagnostics for one parsed script.
pub(crate) fn diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let (Some(hir), Some(path)) = (input.hir.as_deref(), input.path.as_ref()) else {
        return Ok(Vec::new());
    };
    let ir = snapshot.ir();
    let Some((_, file_rule)) = ir.file_rule(path) else {
        return Ok(Vec::new());
    };
    if ir.schemas.is_empty() || file_rule.parser != rules::ir::DocumentParser::Script {
        return Ok(Vec::new());
    }

    schema_diagnostics(snapshot, hir, cancellation)
}

fn schema_diagnostics(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let mut result = schema_diagnostics_at_depth(snapshot, hir, cancellation, 0, false)?;
    if hir
        .analysis_coverage()
        .limits
        .contains(&hir::analysis::AnalysisLimit::FactStability)
    {
        result.push(Diagnostic::new(
            DiagnosticCode::AnalysisIncomplete,
            Severity::Information,
            TextRange::empty(hir.syntax().root().range().start()),
            hir::analysis::AnalysisLimit::FactStability
                .message()
                .to_owned(),
        ));
    }
    Ok(result)
}

/// Rule-only overload groups that can impose an occurrence bound. Keep every
/// overload in a retained group: an unbounded or differently scoped alternative
/// still participates in the effective bounds at each source location.
struct CardinalityGroups(Vec<(MatcherId, Vec<FieldId>)>);

/// Point queries over schema ranges, preserving the linear lookup's shortest
/// containing range and source-order tie break. Subtree end bounds avoid
/// visiting disjoint earlier blocks for every scalar in a large document.
struct SchemaFactIndex<'a> {
    entries: Vec<(usize, &'a hir::SchemaFact)>,
    max_ends: Vec<text::TextSize>,
    leaf_base: usize,
}

struct SchemaPointQuery {
    position: text::TextSize,
    schema: Option<SchemaId>,
    limit: usize,
}

impl<'a> SchemaFactIndex<'a> {
    fn new(facts: &'a [hir::SchemaFact]) -> Self {
        let mut entries = facts.iter().enumerate().collect::<Vec<_>>();
        entries.sort_by_key(|(_, fact)| fact.range.start());
        let leaf_base = entries.len().max(1).next_power_of_two();
        let mut max_ends = vec![0; leaf_base * 2];
        for (index, (_, fact)) in entries.iter().enumerate() {
            max_ends[leaf_base + index] = fact.range.end();
        }
        for node in (1..leaf_base).rev() {
            max_ends[node] = max_ends[node * 2].max(max_ends[node * 2 + 1]);
        }
        Self {
            entries,
            max_ends,
            leaf_base,
        }
    }

    fn at(
        &self,
        position: text::TextSize,
        schema: Option<SchemaId>,
    ) -> Option<&'a hir::SchemaFact> {
        let query = SchemaPointQuery {
            position,
            schema,
            limit: self
                .entries
                .partition_point(|(_, fact)| fact.range.start() <= position),
        };
        let mut best = None;
        self.visit(1, 0..self.leaf_base, &query, &mut best);
        best.map(|(_, fact)| fact)
    }

    fn visit(
        &self,
        node: usize,
        bounds: std::ops::Range<usize>,
        query: &SchemaPointQuery,
        best: &mut Option<(usize, &'a hir::SchemaFact)>,
    ) {
        if bounds.start >= query.limit || self.max_ends[node] < query.position {
            return;
        }
        if bounds.len() == 1 {
            let (index, fact) = self.entries[bounds.start];
            if query.schema.is_none_or(|schema| fact.schema == schema)
                && crate::support::contains(fact.range, query.position)
                && best.is_none_or(|(previous, previous_fact)| {
                    (fact.range.len(), index) < (previous_fact.range.len(), previous)
                })
            {
                *best = Some((index, fact));
            }
            return;
        }
        let middle = bounds.start + bounds.len() / 2;
        self.visit(node * 2, bounds.start..middle, query, best);
        self.visit(node * 2 + 1, middle..bounds.end, query, best);
    }
}

fn cardinality_groups(
    snapshot: &AnalysisSnapshot,
    schema: SchemaId,
) -> std::sync::Arc<CardinalityGroups> {
    let key = format!("ir-cardinality-groups:{}", schema.index());
    if let Some(groups) = snapshot
        .query_cache()
        .get::<CardinalityGroups>(snapshot.revision(), &key)
    {
        return groups;
    }
    let ir = snapshot.ir();
    let mut groups = BTreeMap::<MatcherId, Vec<FieldId>>::new();
    for id in ir.fields(schema) {
        groups.entry(ir.field(id).key).or_default().push(id);
    }
    let groups = std::sync::Arc::new(CardinalityGroups(
        groups
            .into_iter()
            .filter(|(_, ids)| {
                ids.iter()
                    .any(|id| ir.field(*id).card.min != 0 || ir.field(*id).card.max.is_some())
            })
            .collect(),
    ));
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Index,
        key,
        std::sync::Arc::clone(&groups),
    );
    groups
}

fn append_selector_errors(
    ir: &RulesIr,
    failures: impl IntoIterator<Item = (rules::ir::Symbol, rules::query::QueryState)>,
    context: &hir::query::PropertyQueryContext<'_>,
    container: TextRange,
    fallback: TextRange,
    seen: &mut BTreeSet<(TextRange, rules::ir::Symbol)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut failed = false;
    for (selector, state) in failures {
        failed = true;
        if seen.insert((container, selector)) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                context
                    .source_range(ir.strings().resolve(selector))
                    .unwrap_or(fallback),
                hir::checking::query_selector_explanation(ir, selector, state),
            ));
        }
    }
    failed
}

fn schema_diagnostics_at_depth(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
    template_depth: usize,
    partial_root: bool,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let ir = snapshot.ir();
    let facts = WorkspaceFacts { snapshot };
    let schema_index = SchemaFactIndex::new(hir.schema_facts());
    let mut diagnostics = Vec::new();
    let template_arguments = template_argument_key_ranges(snapshot, hir, cancellation)?;
    let mut direct_counts =
        BTreeMap::<(TextRange, FieldId), BTreeMap<String, (u32, TextRange)>>::new();
    let mut unbound_keys = BTreeSet::new();
    let mut selector_errors = BTreeSet::new();
    let properties = hir.properties();
    let query_index = hir::query::PropertyQueryIndex::new(properties);
    for overload in hir
        .overload_facts()
        .iter()
        .filter(|overload| overload.validation == hir::analysis::Validation::Invalid)
    {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::InvalidValue,
            Severity::Error,
            overload.range,
            "no rule overload accepts this complete block".to_owned(),
        ));
    }
    for property in properties {
        cancellation.checkpoint()?;
        let owner_range = (!hir.parameter_references().is_empty())
            .then(|| {
                hir.definitions()
                    .iter()
                    .find(|definition| {
                        definition.range.start() <= property.range.start()
                            && property.range.end() <= definition.range.end()
                            && crate::semantic::dynamic_definition_type(snapshot, &definition.kind)
                    })
                    .map(|definition| definition.range)
            })
            .flatten();
        let binding_dependent = |range: TextRange| {
            owner_range.is_some_and(|owner| {
                hir.parameter_references().iter().any(|reference| {
                    reference.owner_range == owner
                        && range.start() <= reference.range.start()
                        && reference.range.end() <= range.end()
                })
            })
        };
        if binding_dependent(property.key_range) {
            if let Some(field) = hir.field_fact_at(property.key_range)
                && let Some(parent) =
                    schema_index.at(property.key_range.start(), Some(field.schema))
            {
                unbound_keys.insert((parent.range, parent.schema));
            }
            continue;
        }
        if template_arguments.contains(&property.key_range) {
            continue;
        }
        if hir.overload_facts().iter().any(|overload| {
            overload.validation != hir::analysis::Validation::Valid
                && overload.container.start() <= property.key_range.start()
                && property.key_range.end() <= overload.container.end()
        }) {
            continue;
        }
        let Some(field_fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(parent_schema) =
            schema_index.at(property.key_range.start(), Some(field_fact.schema))
        else {
            continue;
        };
        let context = query_index
            .for_property(property)
            .defer_templates(owner_range.is_some());
        let candidates = field_fact.fields.clone();
        if candidates.is_empty() {
            let failures = ir.fields(field_fact.schema).into_iter().flat_map(|id| {
                hir::checking::query_selector_failures(ir, ir.field(id).key, &facts, &context)
            });
            if append_selector_errors(
                ir,
                failures,
                &context,
                parent_schema.range,
                property.key_range,
                &mut selector_errors,
                &mut diagnostics,
            ) {
                continue;
            }
            if ir.fields(field_fact.schema).into_iter().any(|id| {
                hir::checking::scalar_outcome_with_context(
                    ir,
                    ir.field(id).key,
                    &property.key,
                    &facts,
                    &context,
                )
                .0
                .is_none()
            }) {
                continue;
            }
            let known = ir.fields(field_fact.schema).into_iter().find(|id| {
                matcher_matches_with_context(
                    ir,
                    ir.field(*id).key,
                    &property.key,
                    &facts,
                    &parent_schema.state,
                    &context,
                )
            });
            if let Some(id) = known {
                let expected = match ir.shape(id) {
                    Some(Shape::Scalar) => "a scalar value",
                    Some(Shape::Block) => "a block",
                    None => "the declared value shape",
                };
                diagnostics.push(field_diagnostic(
                    ir,
                    field_fact.schema,
                    id,
                    Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        severity(ir.field(id).severity),
                        property.value_range.unwrap_or(property.key_range),
                        format!("`{}` expects {expected}", property.key),
                    ),
                ));
            } else if !ir.schema(field_fact.schema).open
                && let Some(query_matcher) = ir
                    .fields(field_fact.schema)
                    .into_iter()
                    .map(|field| ir.field(field).key)
                    .find(|matcher| has_value_query(ir, *matcher))
            {
                let expected = projected_matchers(ir, query_matcher, snapshot, &context)
                    .into_iter()
                    .map(|(matcher, _)| diagnostic_description(ir, matcher))
                    .collect::<Vec<_>>()
                    .join(" or ");
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        DiagnosticCode::InvalidValue.severity(),
                        property.key_range,
                        format!("invalid branch value `{}`", property.key),
                    )
                    .with_expected(if expected.is_empty() {
                        describe(ir, query_matcher)
                    } else {
                        expected
                    }),
                );
            } else if !ir.schema(field_fact.schema).open {
                let expected = ir
                    .schema(field_fact.schema)
                    .exact
                    .is_empty()
                    .then(|| {
                        ir.fields(field_fact.schema).into_iter().find_map(|id| {
                            let key = ir.field(id).key;
                            matches!(ir.matcher(key), Matcher::Ref(_) | Matcher::Enum { .. })
                                .then(|| describe(ir, key))
                        })
                    })
                    .flatten();
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::UnknownKey,
                    DiagnosticCode::UnknownKey.severity(),
                    property.key_range,
                    misplaced_template_key(snapshot, field_fact.schema, &property.key)
                        .unwrap_or_else(|| {
                            format!(
                                "unknown key `{}`{}{}",
                                property.key,
                                unknown_key_context(snapshot, hir, field_fact.schema, property),
                                expected.map_or(String::new(), |expected| format!(
                                    "; expected {expected}"
                                ))
                            )
                        }),
                ));
            }
            continue;
        }
        let selected = property
            .scalar
            .as_ref()
            .and_then(|scalar| {
                candidates
                    .iter()
                    .copied()
                    .find(|candidate| match ir.field(*candidate).value {
                        FieldValue::Scalar(matcher) => matcher_matches_with_context(
                            ir,
                            matcher,
                            &scalar.value,
                            &facts,
                            &parent_schema.state,
                            &context,
                        ),
                        _ => false,
                    })
            })
            .unwrap_or(candidates[0]);
        let field = ir.field(selected);
        if let (FieldValue::Scalar(matcher), Some(scalar)) = (field.value, &property.scalar) {
            let checked = hir::checking::scalar_validation_cancellable_with_context(
                ir,
                matcher,
                &scalar.value,
                &parent_schema.state,
                &facts,
                &context,
                &mut || cancellation.checkpoint(),
            )?;
            if !checked.coverage.is_complete() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::AnalysisIncomplete,
                    Severity::Information,
                    scalar.range,
                    checked.coverage.limit_description(),
                ));
            }
        }
        direct_counts
            .entry((parent_schema.range, selected))
            .or_default()
            .entry(property.key.to_ascii_lowercase())
            .or_insert((0, property.key_range))
            .0 += 1;
        if field.deprecated {
            let mut diagnostic = Diagnostic::new(
                DiagnosticCode::InvalidValue,
                severity(field.severity),
                property.key_range,
                format!("`{}` is deprecated", property.key),
            );
            diagnostic
                .tags
                .push(crate::types::DiagnosticTag::Deprecated);
            diagnostics.push(diagnostic);
        }
        if let (Some(scope), Some(current)) = (&field.scope, parent_schema.state.current.first())
            && !scope.scopes_in.is_empty()
            && !scope_allows(ir, current, &scope.scopes_in)
        {
            let actual = match current {
                ScopeValue::Known(names) => names
                    .iter()
                    .map(|name| name.as_ref())
                    .collect::<Vec<_>>()
                    .join("/"),
                _ => "any".to_owned(),
            };
            let expected = scope
                .scopes_in
                .iter()
                .map(|scope| format!("`{}`", ir.strings().resolve(*scope)))
                .collect::<Vec<_>>()
                .join(" or ");
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::WrongScope,
                    DiagnosticCode::WrongScope.severity(),
                    property.key_range,
                    format!("`{}` is not available in scope `{actual}`", property.key),
                )
                .with_expected(expected),
            );
        }
        if matches!(ir.matcher(field.key), Matcher::Link)
            && !scope_expression_allowed(
                ir,
                snapshot,
                &property.key,
                None,
                Some(&parent_schema.state),
            )
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::WrongScope,
                DiagnosticCode::WrongScope.severity(),
                property.key_range,
                format!("`{}` is not valid in this scope", property.key),
            ));
        }
        if let (FieldValue::Scalar(matcher), Some(scalar)) = (field.value, property.scalar.as_ref())
            && !binding_dependent(scalar.range)
            && !matcher_matches_with_context(
                ir,
                matcher,
                &scalar.value,
                &facts,
                &parent_schema.state,
                &context,
            )
        {
            let failures = hir::checking::query_selector_failures(ir, matcher, &facts, &context);
            if append_selector_errors(
                ir,
                failures,
                &context,
                parent_schema.range,
                scalar.range,
                &mut selector_errors,
                &mut diagnostics,
            ) {
                continue;
            }
            let wrong_scope =
                matcher_has_scope(ir, matcher) && hir::is_ir_scope_link(ir, &scalar.value);
            let texture = matcher_has_texture_path(ir, matcher);
            let mut diagnostic = Diagnostic::new(
                if wrong_scope {
                    DiagnosticCode::WrongScope
                } else if texture {
                    DiagnosticCode::UnknownTexturePath
                } else {
                    DiagnosticCode::InvalidValue
                },
                if wrong_scope {
                    DiagnosticCode::WrongScope.severity()
                } else {
                    severity(field.severity)
                },
                scalar.range,
                if texture {
                    format!(
                        "texture file `{}` not found in any mod, game, or DLC pack root",
                        scalar.value
                    )
                } else {
                    format!("invalid value `{}` for `{}`", scalar.value, property.key)
                },
            );
            if !texture {
                diagnostic = diagnostic.with_expected(diagnostic_description(ir, matcher));
            }
            if matches!(ir.matcher(matcher), Matcher::Enum { .. }) {
                let names = spellings(ir, matcher, snapshot, "");
                if let Some(candidate) =
                    crate::suggest::best_suggestion(&scalar.value, names.iter().map(String::as_str))
                {
                    diagnostic
                        .message
                        .push_str(&crate::messages::did_you_mean(Some(candidate)));
                    diagnostic = diagnostic.with_fix(crate::QuickFix::suggestion(
                        format!("Did you mean '{candidate}'?"),
                        scalar.range,
                        format!("\"{}\"", parser::encode_quoted_script_text(candidate)),
                    ));
                }
            }
            diagnostics.push(diagnostic);
        }
    }
    for fact in hir.schema_facts() {
        if hir.overload_facts().iter().any(|overload| {
            overload.validation != hir::analysis::Validation::Valid
                && overload.container.start() <= fact.range.start()
                && fact.range.end() <= overload.container.end()
        }) {
            continue;
        }
        cancellation.checkpoint()?;
        let schema = ir.schema(fact.schema);
        // A substituted key can satisfy a required field or pattern only after
        // binding. Keep this deferral local to its immediate enclosing schema.
        let partial = unbound_keys.contains(&(fact.range, fact.schema))
            || (partial_root
                && fact.range.start() == 0
                && fact.range.end() as usize == hir.syntax().source().len());
        if !partial
            && !schema.forms.is_empty()
            && !schema.forms.iter().any(|form| {
                form.counts.iter().all(|(ids, card)| {
                    let count = ids
                        .iter()
                        .filter_map(|id| direct_counts.get(&(fact.range, *id)))
                        .flat_map(|counts| counts.values())
                        .fold(0_u32, |total, (count, _)| total.saturating_add(*count));
                    count >= card.min && card.max.is_none_or(|max| count <= max)
                })
            })
        {
            let forms = schema
                .forms
                .iter()
                .map(|form| {
                    form.counts
                        .iter()
                        .map(|(ids, card)| {
                            let name = ids
                                .first()
                                .map(|id| matcher_key_name(ir, ir.field(*id).key))
                                .unwrap_or_default();
                            let bounds = match card.max {
                                Some(max) if max == card.min => card.min.to_string(),
                                Some(max) => format!("{}..{max}", card.min),
                                None => format!("{}..*", card.min),
                            };
                            format!("`{name}` × {bounds}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect::<Vec<_>>()
                .join("; or ");
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                DiagnosticCode::Cardinality.severity(),
                TextRange::empty(fact.range.start()),
                format!("block must match one of these forms: {forms}"),
            ));
        }
        let overloads = cardinality_groups(snapshot, fact.schema);
        for (key, ids) in &overloads.0 {
            let applicable = ids
                .iter()
                .copied()
                .filter(|id| {
                    let field = ir.field(*id);
                    field.scope.as_ref().is_none_or(|scope| {
                        scope.scopes_in.is_empty()
                            || fact.state.current.first().is_some_and(|current| {
                                matches!(current, ScopeValue::Known(_))
                                    && scope_allows(ir, current, &scope.scopes_in)
                            })
                    })
                })
                .collect::<Vec<_>>();
            if applicable.is_empty() {
                continue;
            }
            let min = applicable
                .iter()
                .map(|id| ir.field(*id).card.min)
                .max()
                .unwrap_or(0);
            let max = if applicable.iter().any(|id| ir.field(*id).card.max.is_none()) {
                None
            } else {
                applicable
                    .iter()
                    .filter_map(|id| ir.field(*id).card.max)
                    .max()
            };
            if min == 0 && max.is_none() {
                continue;
            }
            let mut counts = BTreeMap::<String, (u32, TextRange)>::new();
            for id in &applicable {
                if let Some(occurrences) = direct_counts.get(&(fact.range, *id)) {
                    for (name, (count, first)) in occurrences {
                        let entry = counts.entry(name.clone()).or_insert((0, *first));
                        entry.0 = entry.0.saturating_add(*count);
                    }
                }
            }
            let count = counts
                .values()
                .fold(0_u32, |total, (count, _)| total.saturating_add(*count));
            let anchor = counts
                .values()
                .map(|(_, range)| *range)
                .min_by_key(|range| range.start())
                .unwrap_or(fact.range);
            let field = ir.field(applicable[0]);
            if !partial && count < min {
                let name = matcher_key_name(ir, *key);
                diagnostics.push(field_diagnostic(
                    ir,
                    fact.schema,
                    applicable[0],
                    Diagnostic::new(
                        DiagnosticCode::Cardinality,
                        severity(field.severity),
                        hir.properties()
                            .iter()
                            .find(|property| property.value_range == Some(fact.range))
                            .map_or(TextRange::empty(anchor.start()), |property| {
                                property.key_range
                            }),
                        if min == 1 {
                            format!("this block is missing required key `{name}`")
                        } else {
                            format!("`{name}` must occur at least {min} times")
                        },
                    ),
                ));
            }
            // Pattern cardinality limits repeated instances of the same key,
            // not the entire vocabulary accepted by that pattern.
            for (name, (count, anchor)) in counts {
                if max.is_some_and(|max| count > max) {
                    diagnostics.push(field_diagnostic(
                        ir,
                        fact.schema,
                        applicable[0],
                        Diagnostic::new(
                            DiagnosticCode::Cardinality,
                            severity(field.severity),
                            anchor,
                            format!(
                                "`{name}` may occur at most {} {}",
                                max.unwrap_or_default(),
                                if max == Some(1) { "time" } else { "times" }
                            ),
                        ),
                    ));
                }
            }
        }
    }
    for value in hir.bare_values() {
        cancellation.checkpoint()?;
        let Some(fact) = schema_index.at(value.range.start(), None) else {
            continue;
        };
        let Some(matcher) = ir.schema(fact.schema).items else {
            continue;
        };
        let context = query_index.in_container(fact.range);
        if !matcher_matches_with_context(ir, matcher, &value.value, &facts, &fact.state, &context) {
            let failures = hir::checking::query_selector_failures(ir, matcher, &facts, &context);
            if append_selector_errors(
                ir,
                failures,
                &context,
                fact.range,
                value.range,
                &mut selector_errors,
                &mut diagnostics,
            ) {
                continue;
            }
            let overflow = numeric_range_overflow(ir, matcher, &value.value);
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    if overflow {
                        Severity::Warning
                    } else {
                        DiagnosticCode::InvalidValue.severity()
                    },
                    value.range,
                    if overflow {
                        format!("value `{}` is out of range", value.value)
                    } else {
                        format!("value `{}` is not valid here", value.value)
                    },
                )
                .with_expected(diagnostic_description(ir, matcher)),
            );
        }
    }
    diagnostics.extend(control_lints(ir, hir, &facts, cancellation)?);
    if template_depth < 8 {
        diagnostics.extend(template_argument_diagnostics(
            snapshot,
            hir,
            cancellation,
            template_depth,
        )?);
    } else {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::AnalysisIncomplete,
            Severity::Information,
            hir.syntax().root().range(),
            "Template consumption exceeded the analysis depth limit".to_owned(),
        ));
    }
    for diagnostic in &mut diagnostics {
        if diagnostic.provenance.is_some() || diagnostic.code == DiagnosticCode::UnknownKey {
            continue;
        }
        let owner = properties
            .iter()
            .filter(|property| {
                property.range.start() <= diagnostic.range.start()
                    && diagnostic.range.end() <= property.range.end()
            })
            .min_by_key(|property| property.range.len());
        if let Some(fact) = owner.and_then(|property| hir.field_fact_at(property.key_range))
            && let Some(id) = fact.fields.first()
        {
            diagnostic.provenance = field_provenance(ir, fact.schema, *id);
        }
    }
    Ok(diagnostics)
}

fn template_context(snapshot: &AnalysisSnapshot, schema: SchemaId) -> Option<&str> {
    let ir = snapshot.ir();
    ir.fields(schema).into_iter().find_map(|id| {
        let kind = crate::ir_template::template_kind(ir, ir.field(id).key)?;
        snapshot.rules().dynamic_definition_context(&kind)
    })
}

fn unknown_key_context(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    schema: SchemaId,
    property: &hir::HirProperty,
) -> String {
    if let Some(context) = template_context(snapshot, schema) {
        return format!(" in {} `{context}` block", indefinite_article(context));
    }
    hir.definitions()
        .iter()
        .filter(|definition| {
            definition.range.start() <= property.range.start()
                && property.range.end() <= definition.range.end()
        })
        .min_by_key(|definition| definition.range.len())
        .map_or(String::new(), |definition| {
            format!(
                " in {} `{}` definition",
                indefinite_article(&definition.kind),
                definition.kind
            )
        })
}

fn indefinite_article(name: &str) -> &'static str {
    if name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

fn misplaced_template_key(
    snapshot: &AnalysisSnapshot,
    schema: SchemaId,
    key: &str,
) -> Option<String> {
    let ir = snapshot.ir();
    let current = template_context(snapshot, schema)?;
    for ty in &ir.types {
        let name = ir.strings().resolve(ty.name);
        let Some(other) = snapshot.rules().dynamic_definition_context(name) else {
            continue;
        };
        if other == current {
            continue;
        }
        let Some(schema) = ir.schema_by_name(other) else {
            continue;
        };
        if ir.fields(schema).into_iter().any(|id| matches!(ir.matcher(ir.field(id).key), Matcher::Literal(name) if ir.strings().resolve(*name).eq_ignore_ascii_case(key))) {
            let guard = ir.schema_by_name(current).and_then(|schema| ir.fields(schema).into_iter().find_map(|id| {
                let field = ir.field(id);
                if field.control.as_ref()?.kind != ControlKind::Guard { return None; }
                match ir.matcher(field.key) { Matcher::Literal(name) => Some(ir.strings().resolve(*name)), _ => None }
            }));
            return Some(format!("`{key}` is {} {other} and cannot be used inside {} {current} block{}", indefinite_article(other), indefinite_article(current), guard.map_or(String::new(), |guard| format!("; conditions belong in a `{guard}` block"))));
        }
    }
    None
}

fn matcher_has_texture_path(ir: &RulesIr, matcher: MatcherId) -> bool {
    match ir.matcher(matcher) {
        Matcher::Path(Some(category)) => {
            ir.strings().resolve(*category).eq_ignore_ascii_case("gfx")
        }
        Matcher::Union(items) => items.iter().any(|item| matcher_has_texture_path(ir, *item)),
        _ => false,
    }
}

/// Diagnostic expectations preserve the public message contract. Hover can use
/// the more compact matcher description without changing diagnostic identities.
fn diagnostic_description(ir: &RulesIr, matcher: MatcherId) -> String {
    match ir.matcher(matcher) {
        Matcher::Ref(RefTarget::Type {
            type_id, subtype, ..
        }) => {
            let name = ir.strings().resolve(ir.type_info(*type_id).name);
            subtype.map_or_else(
                || format!("{} `{name}` name", indefinite_article(name)),
                |subtype| {
                    format!(
                        "{} `{}` `{name}` name",
                        indefinite_article(ir.strings().resolve(subtype)),
                        ir.strings().resolve(subtype)
                    )
                },
            )
        }
        Matcher::Int { min, max } => diagnostic_range("whole number", *min, *max),
        Matcher::Float { min, max } => diagnostic_range("number", *min, *max),
        Matcher::Enum { id } => format!(
            "one of {}",
            ir.enum_info(*id)
                .rows
                .iter()
                .map(|row| format!("`{}`", ir.strings().resolve(row.spelling)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Matcher::Union(items) => items
            .iter()
            .map(|item| diagnostic_description(ir, *item))
            .collect::<Vec<_>>()
            .join(", or "),
        _ => describe(ir, matcher),
    }
}

fn diagnostic_range<T: std::fmt::Display>(name: &str, min: Option<T>, max: Option<T>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("a {name} between {min} and {max}"),
        (Some(min), None) => format!("a {name} of at least {min}"),
        (None, Some(max)) => format!("a {name} of at most {max}"),
        (None, None) => format!("a {name}"),
    }
}

fn matcher_has_scope(ir: &RulesIr, matcher: MatcherId) -> bool {
    match ir.matcher(matcher) {
        Matcher::Scope(_) => true,
        Matcher::Union(items) => items.iter().any(|item| matcher_has_scope(ir, *item)),
        _ => false,
    }
}

fn numeric_range_overflow(ir: &RulesIr, matcher: MatcherId, value: &str) -> bool {
    match ir.matcher(matcher) {
        Matcher::Int { min, max } => value.parse::<i64>().is_ok_and(|value| {
            min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max)
        }),
        Matcher::Float { min, max } => value.parse::<f64>().is_ok_and(|value| {
            value.is_finite()
                && (min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max))
        }),
        Matcher::Union(items) => items
            .iter()
            .any(|item| numeric_range_overflow(ir, *item, value)),
        _ => false,
    }
}

fn field_provenance(
    ir: &RulesIr,
    schema: SchemaId,
    field: FieldId,
) -> Option<crate::DiagnosticProvenance> {
    let source = ir.provenance_of(field)?;
    Some(crate::DiagnosticProvenance {
        rule_id: Some(format!("ir:{}:{}", schema.index(), field.index())),
        context: Some(ir.strings().resolve(ir.schema(schema).name).to_owned()),
        source_file: Some(ir.strings().resolve(source.file).to_owned()),
        source_line: None,
        source_pointer: Some(ir.strings().resolve(source.pointer).to_owned()),
    })
}

fn field_diagnostic(
    ir: &RulesIr,
    schema: SchemaId,
    field: FieldId,
    mut diagnostic: Diagnostic,
) -> Diagnostic {
    diagnostic.provenance = field_provenance(ir, schema, field);
    diagnostic
}

/// Checks parameter bindings on blocks invoking a symbol backed by the IR's `Template` trait.
/// The signature comes from HIR's definition/parameter facts; no legacy semantic rule table is
/// consulted on the IR path.
fn template_argument_diagnostics(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
    depth: usize,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let ir = snapshot.ir();
    let Some(template) = ir.trait_by_name("Template") else {
        return Ok(Vec::new());
    };
    let mut diagnostics = Vec::new();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let Some(field_fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(kind) = field_fact.fields.iter().find_map(|field_id| {
            let field = ir.field(*field_id);
            let mut type_ids = Vec::new();
            template_targets(ir, field.key, &mut type_ids);
            type_ids.into_iter().find_map(|type_id| {
                let info = ir.type_info(type_id);
                let implements = info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == template);
                implements.then(|| ir.strings().resolve(info.name).to_owned())
            })
        }) else {
            continue;
        };
        if hir.definitions().iter().any(|definition| {
            definition.kind.eq_ignore_ascii_case(&kind)
                && definition.name.eq_ignore_ascii_case(&property.key)
                && definition.range.start() <= property.range.start()
                && property.range.end() <= definition.range.end()
        }) {
            continue;
        }
        if hir.parameter_references().iter().any(|reference| {
            property.range.start() <= reference.range.start()
                && reference.range.end() <= property.range.end()
        }) {
            continue;
        }
        let Some(summary) =
            crate::semantic::dynamic_definition_summary(snapshot, &kind, &property.key)
        else {
            continue;
        };
        let state = crate::ir_template::invocation_state(hir, property);
        let child_path_len = property.path.len() + 1;
        let arguments = hir
            .properties_in_range(property.range)
            .filter(|child| {
                child.path.len() == child_path_len
                    && child.path.starts_with(&property.path)
                    && property.range.start() <= child.range.start()
                    && child.range.end() <= property.range.end()
            })
            .collect::<Vec<_>>();
        if crate::ir_template::needs_body_analysis(snapshot, &summary)
            && let Some(body) =
                crate::ir_template::analyse_body(snapshot, hir, property, None, cancellation)?
        {
            if !body.coverage.is_complete() {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::AnalysisIncomplete,
                    Severity::Information,
                    property.key_range,
                    body.coverage.limit_description(),
                ));
            }
            for evidence in &body.evidence {
                if evidence.kind == hir::checking::IssueKind::Syntax {
                    let dependencies = crate::ir_template::evidence_dependencies(&body, evidence);
                    let already_in_payload = arguments.iter().any(|argument| {
                        dependencies.contains(&argument.key.to_ascii_lowercase())
                            && argument.scalar.as_ref().is_some_and(|scalar| {
                                scalar.quoted
                                    && hir
                                        .syntax()
                                        .text(scalar.range)
                                        .and_then(parser::parse_quoted_script)
                                        .is_some_and(|script| !script.parsed().errors().is_empty())
                            })
                    });
                    if already_in_payload {
                        continue;
                    }
                }
                let code = constraint_code(evidence.kind);
                let range =
                    crate::ir_template::project_evidence_range(&body, evidence, hir, property);
                diagnostics.push(Diagnostic::new(
                    code,
                    severity(evidence.severity),
                    range,
                    format!("Template `{}`: {}", summary.name, evidence.explanation),
                ));
            }
        }
        let mut counts = BTreeMap::<String, u32>::new();
        for (argument_index, argument) in arguments.iter().enumerate() {
            cancellation.checkpoint()?;
            // The game's last scalar binding wins; earlier duplicate values still get the
            // duplicate warning, but must not produce a stale usage-site error.
            let shadowed = arguments[argument_index + 1..].iter().any(|later| {
                later.key.eq_ignore_ascii_case(&argument.key) && later.scalar.is_some()
            });
            if let Some(scalar) = argument.scalar.as_ref().filter(|_| !shadowed) {
                let sites = crate::ir_template::parameter_sites_at(
                    snapshot,
                    hir,
                    property,
                    &kind,
                    &property.key,
                    &argument.key,
                    cancellation,
                )?;
                if !sites.coverage.is_complete() {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::AnalysisIncomplete,
                        Severity::Information,
                        argument.key_range,
                        sites.coverage.limit_description(),
                    ));
                }
                if !scalar.quoted
                    && sites.iter().any(|site| {
                        matches!(site.domain, crate::ir_template::Domain::Template { schema,.. } if ir.schema(schema).items.is_none())
                    })
                {
                    diagnostics.push(Diagnostic::new(DiagnosticCode::InvalidValue, Severity::Warning,
                        scalar.range, format!("parameter `{}` of scripted `{}` is spliced into a quoted script payload; provide its value as a quoted script", argument.key, summary.name)));
                }
                if scalar.quoted
                    && sites.iter().any(|site| {
                        matches!(site.domain, crate::ir_template::Domain::Template { .. })
                    })
                {
                    let source = hir.syntax().source();
                    if let Some(raw) =
                        source.get(scalar.range.start() as usize..scalar.range.end() as usize)
                    {
                        let mut session = TemplateParseSession::new(cancellation);
                        let parsed_payload = session.parse(raw, depth)?;
                        if let TemplateParse::Limited(reason) = &parsed_payload {
                            diagnostics.push(Diagnostic::new(
                                DiagnosticCode::AnalysisIncomplete,
                                Severity::Information,
                                scalar.range,
                                reason.message().to_owned(),
                            ));
                        }
                        if let TemplateParse::Parsed(script) = parsed_payload {
                            // Syntax belongs to the payload itself, independent of schema
                            // overloads or the number of sites that consume this argument.
                            for error in script.parsed().errors() {
                                cancellation.checkpoint()?;
                                let mut diagnostic =
                                    crate::diagnostics::diagnostic_from_syntax(error);
                                if let Some(relative) =
                                    script.source_map().decoded_range(diagnostic.range)
                                    && let Some(range) = TextRange::new(
                                        scalar.range.start() + relative.start(),
                                        scalar.range.start() + relative.end(),
                                    )
                                {
                                    diagnostic.range = range;
                                    diagnostics.push(diagnostic);
                                }
                            }
                        }
                    }
                }
                let invalid = sites
                    .iter()
                    .filter(|site| !site.accepts(snapshot, &argument.key, &scalar.value))
                    .collect::<Vec<_>>();
                if !invalid.is_empty() {
                    let key_site = invalid.iter().find_map(|site| match site.domain {
                        crate::ir_template::Domain::Key { schema, .. } => Some(schema),
                        _ => None,
                    });
                    let reason = key_site.map_or_else(
                        || "does not match its usage in the definition body".to_owned(),
                        |schema| {
                            format!(
                                "does not name a known {} key",
                                ir.strings().resolve(ir.schema(schema).name)
                            )
                        },
                    );
                    let rendered = invalid.iter().find_map(|site| {
                        site.rendered_value(&argument.key, &scalar.value)
                            .filter(|value| value != &scalar.value)
                    });
                    let reason = rendered.map_or(reason.clone(), |rendered| {
                        format!("renders as `{rendered}` at its usage site, which {reason}")
                    });
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        DiagnosticCode::InvalidValue.severity(),
                        scalar.range,
                        format!(
                            "argument `{}` for parameter `{}` of scripted `{}` {reason}",
                            scalar.value, argument.key, summary.name
                        ),
                    ));
                }
            } else if argument.scalar.is_none()
                && argument.value_range.is_some_and(|range| {
                    hir.syntax()
                        .text(range)
                        .is_some_and(|text| text.trim_start().starts_with('{'))
                })
            {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    DiagnosticCode::InvalidValue.severity(),
                    argument.value_range.unwrap_or(argument.range),
                    format!("parameter `{}` of scripted `{}` is used as a scalar value in its body and must be provided as one", argument.key, summary.name),
                ));
            }
            let count = counts.entry(argument.key.to_ascii_lowercase()).or_default();
            *count = count.saturating_add(1);
            if *count > 1 {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::Cardinality,
                    Severity::Warning,
                    argument.key_range,
                    format!(
                        "dynamic parameter `{}` is provided more than once",
                        argument.key
                    ),
                ));
            }
            if !summary
                .parameters
                .iter()
                .any(|parameter| parameter.name.eq_ignore_ascii_case(&argument.key))
            {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::UnknownKey,
                    DiagnosticCode::UnknownKey.severity(),
                    argument.key_range,
                    format!(
                        "unexpected parameter `{}` of scripted `{}` (known: {})",
                        argument.key,
                        summary.name,
                        summary
                            .parameters
                            .iter()
                            .map(|parameter| parameter.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
        let inputs = crate::ir_template::invocation_inputs(hir, property);
        let required = hir::template::required_parameters_with_inputs(
            snapshot.ir(),
            &WorkspaceFacts { snapshot },
            &kind,
            &property.key,
            &inputs,
            state,
            &mut || cancellation.checkpoint(),
        )?;
        if !required.coverage.is_complete() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::AnalysisIncomplete,
                Severity::Information,
                property.key_range,
                required.coverage.limit_description(),
            ));
        }
        let mut unconditional = required.unconditional.clone();
        let mut missing = required.missing.clone();
        // Preserve signature-only diagnostics when the Template representation is unavailable.
        // Concrete Template invocations and callable queries share the HIR inference above.
        if summary.template.is_none() {
            unconditional.extend(
                summary
                    .parameters
                    .iter()
                    .filter(|parameter| {
                        parameter.required
                            && !inputs
                                .present
                                .contains(&parameter.name.to_ascii_lowercase())
                    })
                    .map(|parameter| parameter.name.clone()),
            );
            missing.extend(unconditional.iter().cloned());
        }
        if !missing.is_empty() {
            let listed = missing
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let message = if unconditional.is_empty() {
                format!(
                    "dynamic definition `{}` requires parameter(s) {listed} in the active branch",
                    summary.name
                )
            } else {
                format!(
                    "dynamic definition `{}` is missing required parameter(s): {listed}{}",
                    summary.name,
                    if property.scalar.is_some() {
                        "; provide them in a parameter block"
                    } else {
                        ""
                    }
                )
            };
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                DiagnosticCode::Cardinality.severity(),
                property.key_range,
                message,
            ));
        }
    }
    Ok(diagnostics)
}

fn template_argument_key_ranges(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
) -> Result<std::collections::BTreeSet<TextRange>, Cancelled> {
    let ir = snapshot.ir();
    let Some(template) = ir.trait_by_name("Template") else {
        return Ok(Default::default());
    };
    let mut ranges = std::collections::BTreeSet::new();
    let mut invocation_paths = BTreeSet::new();
    for invocation in hir.properties() {
        cancellation.checkpoint()?;
        if invocation.scalar.is_some() {
            continue;
        }
        let Some(field_fact) = hir.field_fact_at(invocation.key_range) else {
            continue;
        };
        let kind = field_fact.fields.iter().find_map(|field_id| {
            let mut targets = Vec::new();
            template_targets(ir, ir.field(*field_id).key, &mut targets);
            targets.into_iter().find_map(|type_id| {
                let info = ir.type_info(type_id);
                let implements = info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == template);
                implements.then(|| ir.strings().resolve(info.name).to_owned())
            })
        });
        let Some(kind) = kind else {
            continue;
        };
        if hir.definitions().iter().any(|definition| {
            definition.kind.eq_ignore_ascii_case(&kind)
                && definition.name.eq_ignore_ascii_case(&invocation.key)
                && definition.range.start() <= invocation.range.start()
                && invocation.range.end() <= definition.range.end()
        }) {
            continue;
        }
        // Ambiguous definitions have no reliable signature. Keep their
        // argument block open and let Template validation use a signature
        // only when one is available.
        invocation_paths.insert(invocation.path.as_slice());
    }
    if !invocation_paths.is_empty() {
        for property in hir.properties() {
            cancellation.checkpoint()?;
            if property
                .path
                .split_last()
                .is_some_and(|(_, parent)| invocation_paths.contains(parent))
            {
                ranges.insert(property.key_range);
            }
        }
    }
    Ok(ranges)
}

fn template_targets(ir: &RulesIr, matcher: MatcherId, out: &mut Vec<TypeId>) {
    match ir.matcher(matcher) {
        Matcher::Ref(RefTarget::Type { type_id, .. }) | Matcher::Def { type_id, .. } => {
            out.push(*type_id)
        }
        Matcher::Union(alternatives) => {
            for alternative in alternatives {
                template_targets(ir, *alternative, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn scope_allows(
    ir: &RulesIr,
    current: &ScopeValue,
    expected: &[rules::ir::Symbol],
) -> bool {
    match current {
        ScopeValue::Unknown => true,
        ScopeValue::Invalid => false,
        ScopeValue::Known(names) => names.iter().any(|name| {
            ir.strings().lookup_folded(name).is_some_and(|actual| {
                expected
                    .iter()
                    .any(|expected| ir.scopes_compatible(actual, *expected))
            })
        }),
    }
}

fn severity(value: RuleSeverity) -> Severity {
    match value {
        RuleSeverity::Error => Severity::Error,
        RuleSeverity::Warning => Severity::Warning,
        RuleSeverity::Info => Severity::Information,
    }
}

fn matcher_key_name(ir: &RulesIr, matcher: MatcherId) -> String {
    match ir.matcher(matcher) {
        Matcher::Literal(value) => ir.strings().resolve(*value).to_owned(),
        _ => describe(ir, matcher),
    }
}

pub(crate) fn field_context(ir: &RulesIr, schema: SchemaId, field: FieldId) -> String {
    let origin = ir
        .provenance_of(field)
        .map(|origin| ir.strings().resolve(origin.pointer));
    let name = origin
        .and_then(|pointer| {
            pointer
                .strip_prefix("/schemas/")
                .or_else(|| pointer.strip_prefix("/mixins/"))
        })
        .and_then(|path| path.split('/').next())
        .unwrap_or_else(|| ir.strings().resolve(ir.schema(schema).name));
    let name = name
        .strip_prefix("keys__")
        .unwrap_or(name)
        .strip_prefix("type_")
        .unwrap_or(name.strip_prefix("keys__").unwrap_or(name));
    let name = name.split("__").next().unwrap_or(name);
    name.strip_suffix("_body")
        .or_else(|| name.strip_suffix("_file"))
        .unwrap_or(name)
        .to_owned()
}

fn control_lints(
    ir: &RulesIr,
    hir: &HirFile,
    facts: &impl SymbolFacts,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    hir::checking::control_lints(ir, hir, facts, None, &mut || cancellation.checkpoint()).map(
        |evidence| {
            evidence
                .into_iter()
                .map(diagnostic_from_constraint)
                .collect()
        },
    )
}

fn diagnostic_from_constraint(evidence: hir::checking::ConstraintEvidence) -> Diagnostic {
    let code = constraint_code(evidence.kind);
    Diagnostic::new(
        code,
        severity(evidence.severity),
        evidence.range,
        evidence.explanation,
    )
}

fn constraint_code(kind: hir::checking::IssueKind) -> DiagnosticCode {
    use hir::checking::IssueKind;
    match kind {
        IssueKind::Syntax => DiagnosticCode::Syntax,
        IssueKind::Key => DiagnosticCode::UnknownKey,
        IssueKind::Value => DiagnosticCode::InvalidValue,
        IssueKind::Scope => DiagnosticCode::WrongScope,
        IssueKind::Cardinality => DiagnosticCode::Cardinality,
        IssueKind::OrphanElse => DiagnosticCode::OrphanElse,
        IssueKind::LogicalContainer => DiagnosticCode::LogicalContainer,
        IssueKind::ConstantCondition => DiagnosticCode::ConstantCondition,
        IssueKind::MissingLimit => DiagnosticCode::MissingLimit,
        IssueKind::EmptyBlock => DiagnosticCode::EmptyBlock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{AnalysisHost, DocumentId};
    use text::TextSize;

    #[test]
    fn schema_range_index_preserves_linear_boundaries_filters_and_ties() {
        let ir = game::eu4::first_party_ir().unwrap();
        let effect = ir.schema_by_name("effect").unwrap();
        let trigger = ir.schema_by_name("trigger").unwrap();
        let facts = [
            (10, 20, effect),
            (0, 32, trigger),
            (5, 15, effect),
            (10, 20, trigger),
            (10, 10, effect),
            (21, 25, effect),
            (5, 15, effect),
            (32, 32, trigger),
        ]
        .map(|(start, end, schema)| hir::SchemaFact {
            range: TextRange::new(start, end).unwrap(),
            schema,
            subtypes: Default::default(),
            state: ScopeState {
                root: ScopeValue::Unknown,
                current: vec![ScopeValue::Unknown],
                from: Vec::new(),
                previous: Vec::new(),
            },
        });
        let index = SchemaFactIndex::new(&facts);
        for position in 0..=33 {
            for schema in [None, Some(effect), Some(trigger)] {
                let expected = facts
                    .iter()
                    .filter(|fact| {
                        schema.is_none_or(|schema| fact.schema == schema)
                            && crate::support::contains(fact.range, position)
                    })
                    .min_by_key(|fact| fact.range.len());
                let actual = index.at(position, schema);
                assert_eq!(
                    actual.map(std::ptr::from_ref),
                    expected.map(std::ptr::from_ref),
                    "{position} {schema:?}"
                );
            }
        }
        assert!(SchemaFactIndex::new(&[]).at(0, None).is_none());
    }

    #[test]
    fn production_diagnostics_and_completion_use_first_party_ir_facts() {
        let ir = game::eu4::first_party_ir().expect("first-party IR");
        let rules = game::eu4::runtime_rules().expect("first-party rules");
        let profile = game::eu4::profile();
        let mut host = AnalysisHost::with_ir(rules, profile, ir);
        let id = DocumentId::new("file:///tmp/common/events/phase4-ir-test.txt");
        host.open_document(
            id.clone(),
            1,
            "\nunrecognized_phase4_key = yes".to_owned(),
            None,
        )
        .expect("open fixture");
        let snapshot = host.snapshot();
        let input = crate::support::input_for_document(&snapshot, &id).expect("parsed fixture");
        assert!(has_ir_schema(&snapshot, &input));

        let diagnostics = crate::diagnostics::diagnostics(&snapshot, &id);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownKey)
        );

        let completion = crate::completion::complete(&snapshot, &id, TextSize::from(0_u32));
        assert!(!completion.items.is_empty());

        let value_id = DocumentId::new("file:///tmp/common/events/phase4-empty-rhs.txt");
        let value_source =
            "country_event = { id = phase4.empty_rhs option = { name = phase4.option } hidden = ";
        host.open_document(value_id.clone(), 2, value_source.to_owned(), None)
            .expect("open incomplete scalar fixture");
        let value_snapshot = host.snapshot();
        let value_cursor = u32::try_from(value_source.len()).expect("source position");
        let value_completion =
            crate::completion::complete(&value_snapshot, &value_id, TextSize::from(value_cursor));
        assert!(
            value_completion
                .items
                .iter()
                .any(|item| item.label == "yes"),
            "empty scalar RHS should offer matcher completions: {:?}",
            value_completion.items
        );

        let template_definition =
            DocumentId::new("file:///tmp/common/scripted_effects/phase4-template.txt");
        host.open_document(
            template_definition,
            1,
            "phase4_template = { add_prestige = $AMOUNT$ }".to_owned(),
            None,
        )
        .expect("open Template definition");
        let call_source = "country_event = { id = phase4.call immediate = { phase4_template = {  } } option = { name = phase4.option } }";
        let call_id = DocumentId::new("file:///tmp/common/events/phase4-template-site.txt");
        host.open_document(call_id.clone(), 2, call_source.to_owned(), None)
            .expect("open Template invocation");
        let call_diagnostics = crate::diagnostics::diagnostics(&host.snapshot(), &call_id);
        assert!(
            call_diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DiagnosticCode::Cardinality
                    && diagnostic.message.contains("AMOUNT")
            }),
            "required Template parameter should be diagnosed: {call_diagnostics:#?}"
        );

        let valid_id = DocumentId::new("file:///tmp/common/events/phase4-valid-event.txt");
        let valid_source = "country_event = { id = phase4.valid is_triggered_only = yes option = { name = phase4.option } immediate = {  } }";
        host.open_document(valid_id.clone(), 3, valid_source.to_owned(), None)
            .expect("open valid fixture");
        let valid_snapshot = host.snapshot();
        let valid_diagnostics = crate::diagnostics::diagnostics(&valid_snapshot, &valid_id);
        assert!(
            !valid_diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.code,
                DiagnosticCode::Cardinality
                    | DiagnosticCode::UnknownKey
                    | DiagnosticCode::InvalidValue
            )),
            "unexpected IR diagnostics for a valid event: {valid_diagnostics:#?}"
        );
        let cursor = u32::try_from(valid_source.find("  }").expect("empty immediate block") + 1)
            .expect("source position");
        let nested =
            crate::completion::complete(&valid_snapshot, &valid_id, TextSize::from(cursor));
        assert!(nested.items.iter().any(|item| item.label == "add_prestige"));
    }
}
