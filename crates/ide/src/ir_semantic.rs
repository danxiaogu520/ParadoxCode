//! IDE queries backed directly by the lowered Rules IR.
//!
//! HIR owns the source-to-schema walk. This module only interprets its cached
//! facts and does not rebuild semantic context from paths or legacy rules.

use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::types::{CancellationToken, Cancelled, Diagnostic, DiagnosticCode, Severity};
use crate::{semantic::effective_workspace_member_names, support::ParsedInput};
use engine::AnalysisSnapshot;
use hir::{
    HirFile, ScopeState, ScopeValue, TemplateFragment, TemplateItem, TemplateToken, TemplateValue,
};
use parser::QuotedScript;
use rules::ir::{
    FieldId, FieldValue, Matcher, MatcherId, RefTarget, RulesIr, ScalarFields, SchemaId, Shape,
    SymbolFacts, TemplatePart, TypeId,
};
use rules::source::{ControlKind, Severity as RuleSeverity};
use std::collections::BTreeMap;
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

/// Matches a complete IR matcher, including workspace-backed reference and trait variants.
pub(crate) fn matcher_matches(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
) -> bool {
    match ir.matcher(matcher) {
        Matcher::Def { .. } => !value.is_empty(),
        Matcher::Union(alternatives) => alternatives
            .iter()
            .any(|alternative| matcher_matches(ir, *alternative, value, facts)),
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            if strip_prefix.is_some_and(|prefix| {
                value
                    .get(..ir.strings().resolve(prefix).len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(ir.strings().resolve(prefix)))
            }) {
                return false;
            }
            let is_member = |candidate: &str| {
                subtype.map_or_else(
                    || facts.type_member(*type_id, candidate),
                    |subtype| facts.type_subtype_member(*type_id, subtype, candidate),
                )
            };
            is_member(value)
                || strip_prefix.is_some_and(|prefix| {
                    let mut candidate = ir.strings().resolve(prefix).to_owned();
                    candidate.push_str(value);
                    is_member(&candidate)
                })
        }
        Matcher::Template(parts) => matcher_template_matches(ir, parts, value, facts),
        _ => ir.scalar_matches(matcher, value, facts),
    }
}

pub(crate) fn matcher_matches_in_snapshot(
    snapshot: &AnalysisSnapshot,
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
) -> bool {
    match ir.matcher(matcher) {
        Matcher::Path(Some(category))
            if ir.strings().resolve(*category).eq_ignore_ascii_case("gfx") =>
        {
            snapshot.resolve_texture_path(value).is_some()
        }
        Matcher::Union(alternatives) => alternatives.iter().any(|alternative| {
            matcher_matches_in_snapshot(snapshot, ir, *alternative, value, facts)
        }),
        _ => matcher_matches(ir, matcher, value, facts),
    }
}

fn matcher_matches_with_state(
    snapshot: &AnalysisSnapshot,
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &impl SymbolFacts,
    state: &ScopeState,
) -> bool {
    match ir.matcher(matcher) {
        Matcher::Scope(expected) => {
            scope_expression_allowed(ir, snapshot, value, *expected, Some(state))
        }
        Matcher::Link => scope_expression_allowed(ir, snapshot, value, None, Some(state)),
        Matcher::Union(alternatives) => alternatives.iter().any(|alternative| {
            matcher_matches_with_state(snapshot, ir, *alternative, value, facts, state)
        }),
        _ => matcher_matches_in_snapshot(snapshot, ir, matcher, value, facts),
    }
}

fn matcher_template_matches(
    ir: &RulesIr,
    parts: &[TemplatePart],
    value: &str,
    facts: &impl SymbolFacts,
) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return value.is_empty();
    };
    match first {
        TemplatePart::Text(text) => {
            let text = ir.strings().resolve(*text);
            value.len() >= text.len()
                && value.is_char_boundary(text.len())
                && value[..text.len()].eq_ignore_ascii_case(text)
                && matcher_template_matches(ir, rest, &value[text.len()..], facts)
        }
        TemplatePart::Hole(hole) => value
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .chain(std::iter::once(value.len()))
            .any(|end| {
                matcher_matches(ir, *hole, &value[..end], facts)
                    && matcher_template_matches(ir, rest, &value[end..], facts)
            }),
    }
}

/// Human-readable description of a complete IR matcher.
pub(crate) fn describe(ir: &RulesIr, matcher: MatcherId) -> String {
    match ir.matcher(matcher) {
        Matcher::Scalar => "any scalar".into(),
        Matcher::Literal(value) => format!("`{}`", ir.strings().resolve(*value)),
        Matcher::Template(parts) => format!("template `{}`", template_text(ir, parts)),
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
        Matcher::Ref(RefTarget::Trait(trait_id)) => {
            format!(
                "an instance implementing `{}`",
                ir.strings().resolve(ir.trait_info(*trait_id).name)
            )
        }
        Matcher::Def { type_id, subtype } => {
            format!("a definition of {}", describe_type(ir, *type_id, *subtype))
        }
        Matcher::Enum { id, rows } => {
            let info = ir.enum_info(*id);
            let values = info
                .rows
                .iter()
                .enumerate()
                .filter(|(index, _)| rows.as_ref().is_none_or(|set| set.contains(*index)))
                .map(|(_, row)| ir.strings().resolve(row.name))
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
        Matcher::Quoted(schema) => format!(
            "a quoted script using `{}`",
            ir.strings().resolve(ir.schema(*schema).name)
        ),
        Matcher::Opaque => "text".into(),
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
    spellings_into(ir, matcher, snapshot, prefix, &mut out);
    out.sort_by_key(|value| value.to_ascii_lowercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

pub(crate) fn spellings_with_state(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    state: Option<&ScopeState>,
) -> Vec<String> {
    let mut values = spellings(ir, matcher, snapshot, prefix);
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
    let current = state.and_then(|state| state.current.first());
    if let Some(link) = ir.scopes.links.iter().find(|link| {
        matcher_template_matches(ir, &link.pattern, name, &WorkspaceFacts { snapshot })
    }) {
        let target_ok = expected.is_none_or(|expected| match link.to {
            rules::ir::ScopeRef::Any => true,
            rules::ir::ScopeRef::Type(actual) => ir.scopes_compatible(actual, expected),
        });
        return target_ok
            && current.is_none_or(|current| {
                matches!(current, ScopeValue::Unknown)
                    || link.from.iter().any(|from| match (from, current) {
                        (rules::ir::ScopeRef::Any, _) => true,
                        (rules::ir::ScopeRef::Type(expected), ScopeValue::Known(names)) => {
                            names.iter().any(|name| {
                                ir.strings()
                                    .lookup_folded(name)
                                    .is_some_and(|actual| ir.scopes_compatible(actual, *expected))
                            })
                        }
                        _ => false,
                    })
            });
    }
    if let Some(expected) = expected {
        if ir
            .strings()
            .lookup_folded(name)
            .is_some_and(|actual| ir.scopes_compatible(actual, expected))
        {
            return true;
        }
        if ir.scopes.registers.iter().any(|register| {
            ir.strings()
                .resolve(register.name)
                .eq_ignore_ascii_case(name)
                || (register.chain
                    && register_chain_depth(name, ir.strings().resolve(register.name)).is_some())
        }) {
            return if let Some(state) = state {
                scope_state_register_matches(ir, state, name, expected)
            } else {
                true
            };
        }
        return ["this", "root", "prev", "from"].iter().any(|prefix| {
            (name.eq_ignore_ascii_case(prefix) || register_chain_depth(name, prefix).is_some())
                && state
                    .is_some_and(|state| scope_state_register_matches(ir, state, name, expected))
        });
    }
    ir.scope_matches(None, name)
        || ir.scopes.registers.iter().any(|register| {
            let base = ir.strings().resolve(register.name);
            name.eq_ignore_ascii_case(base)
                || (register.chain && register_chain_depth(name, base).is_some())
        })
}

fn scope_state_register_matches(
    ir: &RulesIr,
    state: &ScopeState,
    register: &str,
    expected: rules::ir::Symbol,
) -> bool {
    let (base, depth) = ["this", "root", "prev", "from"]
        .into_iter()
        .find_map(|base| register_chain_depth(register, base).map(|depth| (base, depth)))
        .unwrap_or((register, 1));
    let value = if base.eq_ignore_ascii_case("root") {
        Some(&state.root)
    } else if base.eq_ignore_ascii_case("this") {
        state.current.first()
    } else if base.eq_ignore_ascii_case("prev") {
        state.previous.get(depth.saturating_sub(1))
    } else if base.eq_ignore_ascii_case("from") {
        state.from.get(depth.saturating_sub(1))
    } else {
        None
    };
    value.is_none_or(|value| match value {
        ScopeValue::Unknown => true,
        ScopeValue::Invalid => false,
        ScopeValue::Known(names) => names.iter().any(|name| {
            ir.strings()
                .lookup_folded(name)
                .is_some_and(|name| ir.scopes_compatible(name, expected))
        }),
    })
}

fn register_chain_depth(name: &str, base: &str) -> Option<usize> {
    if name.eq_ignore_ascii_case(base) {
        return Some(1);
    }
    (2..=4).find(|&depth| {
        name.eq_ignore_ascii_case(&base.repeat(depth))
            || name.eq_ignore_ascii_case(
                &std::iter::repeat_n(base, depth)
                    .collect::<Vec<_>>()
                    .join("_"),
            )
    })
}

fn spellings_into(
    ir: &RulesIr,
    matcher: MatcherId,
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    out: &mut Vec<String>,
) {
    match ir.matcher(matcher) {
        Matcher::Literal(value) => out.push(ir.strings().resolve(*value).to_owned()),
        Matcher::Template(parts) => template_spellings(ir, parts, snapshot, prefix, out),
        Matcher::Bool => out.extend(["yes".into(), "no".into()]),
        Matcher::Date => {}
        Matcher::Enum { id, rows } => {
            let info = ir.enum_info(*id);
            out.extend(
                info.rows
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| rows.as_ref().is_none_or(|set| set.contains(*index)))
                    .map(|(_, row)| ir.strings().resolve(row.name).to_owned()),
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
                        scope.is_none_or(|expected| {
                            ir.scopes.links.iter().any(|link| {
                                link.to
                                    .type_name()
                                    .is_some_and(|actual| ir.scopes_compatible(actual, expected))
                                    && scope_link_spellings(ir, link, snapshot)
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
        Matcher::Ref(RefTarget::Trait(trait_id)) => {
            let facts = WorkspaceFacts { snapshot };
            for info in &ir.types {
                if info.trait_impls.iter().any(|imp| imp.trait_id == *trait_id) {
                    out.extend(
                        info.builtin
                            .iter()
                            .map(|member| ir.strings().resolve(*member).to_owned()),
                    );
                    out.extend(effective_workspace_member_names(
                        snapshot,
                        ir.strings().resolve(info.name),
                    ));
                }
                let Some(type_id) = ir.type_by_name(ir.strings().resolve(info.name)) else {
                    continue;
                };
                for subtype in info.subtypes.iter().filter(|subtype| {
                    subtype
                        .trait_impls
                        .iter()
                        .any(|implementation| implementation.trait_id == *trait_id)
                }) {
                    out.extend(
                        effective_workspace_member_names(snapshot, ir.strings().resolve(info.name))
                            .into_iter()
                            .filter(|member| {
                                facts.type_subtype_member(type_id, subtype.name, member)
                            }),
                    );
                }
            }
        }
        Matcher::Def { type_id, .. } => {
            let name = ir.strings().resolve(ir.type_info(*type_id).name);
            out.extend(effective_workspace_member_names(snapshot, name));
        }
        Matcher::Union(items) => {
            for item in items {
                spellings_into(ir, *item, snapshot, prefix, out);
            }
        }
        Matcher::Scalar
        | Matcher::Int { .. }
        | Matcher::Float { .. }
        | Matcher::Loc
        | Matcher::Path(_)
        | Matcher::Quoted(_)
        | Matcher::Opaque => {}
        Matcher::Link => out.extend(scope_link_and_register_names(ir, snapshot)),
    }
}

fn template_spellings(
    ir: &RulesIr,
    parts: &[TemplatePart],
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    out: &mut Vec<String>,
) {
    let mut variants = vec![String::new()];
    for part in parts {
        let options = match part {
            TemplatePart::Text(text) => vec![ir.strings().resolve(*text).to_owned()],
            TemplatePart::Hole(matcher) => {
                let mut values = Vec::new();
                spellings_into(ir, *matcher, snapshot, "", &mut values);
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
            .starts_with(&prefix.to_ascii_lowercase())
    }));
}

fn scope_link_and_register_names(ir: &RulesIr, snapshot: &AnalysisSnapshot) -> Vec<String> {
    let mut names = vec![
        "this".to_owned(),
        "root".to_owned(),
        "prev".to_owned(),
        "from".to_owned(),
    ];
    for link in ir.scopes.links.iter() {
        names.extend(scope_link_spellings(ir, link, snapshot));
    }
    for register in ir.scopes.registers.iter() {
        let name = ir.strings().resolve(register.name);
        names.push(name.to_owned());
        if register.chain {
            for depth in 2..=4 {
                names.push(name.repeat(depth));
                names.push(
                    std::iter::repeat_n(name, depth)
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
            TemplatePart::Text(text) => vec![ir.strings().resolve(*text).to_owned()],
            TemplatePart::Hole(matcher) => {
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
                    _ => spellings_into(ir, *matcher, snapshot, "", &mut options),
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

fn template_text(ir: &RulesIr, parts: &[TemplatePart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            TemplatePart::Text(text) => ir.strings().resolve(*text).to_owned(),
            TemplatePart::Hole(_) => "{…}".into(),
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
    let mut diagnostics = schema_diagnostics(snapshot, hir, cancellation)?;
    let mut session = QuotedScriptSession::new(cancellation);
    quoted_schema_diagnostics(
        snapshot,
        hir,
        cancellation,
        &mut session,
        &mut Vec::new(),
        &mut diagnostics,
    )?;
    Ok(diagnostics)
}

fn schema_diagnostics(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let ir = snapshot.ir();
    let facts = WorkspaceFacts { snapshot };
    let mut diagnostics = Vec::new();
    let callable_arguments = callable_argument_key_ranges(snapshot, hir, cancellation)?;
    let mut direct_counts =
        BTreeMap::<(TextRange, FieldId), BTreeMap<String, (u32, TextRange)>>::new();
    let properties = hir.properties();
    for property in properties {
        cancellation.checkpoint()?;
        if callable_arguments.contains(&property.key_range) {
            continue;
        }
        let Some(field_fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(parent_schema) =
            ir.schema_facts_for_range(hir, field_fact.schema, property.key_range)
        else {
            continue;
        };
        let candidates = field_fact.fields.clone();
        if candidates.is_empty() {
            let known = ir
                .fields(field_fact.schema, &field_fact.subtypes)
                .into_iter()
                .find(|id| matcher_matches(ir, ir.field(*id).key, &property.key, &facts));
            if let Some(id) = known {
                let expected = match ir.shape(id) {
                    Some(Shape::Scalar) => "a scalar value",
                    Some(Shape::Block) => "a block",
                    Some(Shape::Quoted) => "a quoted script",
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
            } else if !ir.schema(field_fact.schema).open {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::UnknownKey,
                    DiagnosticCode::UnknownKey.severity(),
                    property.key_range,
                    format!("unknown key `{}`", property.key),
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
                        FieldValue::Scalar(matcher) => matcher_matches_with_state(
                            snapshot,
                            ir,
                            matcher,
                            &scalar.value,
                            &facts,
                            &parent_schema.state,
                        ),
                        _ => false,
                    })
            })
            .unwrap_or(candidates[0]);
        let field = ir.field(selected);
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::WrongScope,
                DiagnosticCode::WrongScope.severity(),
                property.key_range,
                format!("`{}` is not valid in this scope", property.key),
            ));
        }
        if let (FieldValue::Scalar(matcher), Some(scalar)) = (field.value, property.scalar.as_ref())
            && !matcher_matches_with_state(
                snapshot,
                ir,
                matcher,
                &scalar.value,
                &facts,
                &parent_schema.state,
            )
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                severity(field.severity),
                scalar.range,
                format!("expected {}", describe(ir, matcher)),
            ));
        }
    }
    for fact in hir.schema_facts() {
        cancellation.checkpoint()?;
        let mut overloads = BTreeMap::<MatcherId, Vec<FieldId>>::new();
        for id in ir.fields(fact.schema, &fact.subtypes) {
            overloads.entry(ir.field(id).key).or_default().push(id);
        }
        for (key, ids) in overloads {
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
            let field = ir.field(applicable[0]);
            if count < min {
                let owned_guard = hir.properties().iter().find(|property| property.value_range == Some(fact.range)).and_then(|property| {
                    hir.field_fact_at(property.key_range)
                }).is_some_and(|owner| owner.fields.iter().any(|id| {
                    ir.field(*id).control.as_ref().and_then(|control| control.guard).is_some_and(|guard| {
                        matches!(ir.matcher(key), Matcher::Literal(name) if *name == guard)
                    })
                }));
                if !owned_guard {
                    let name = matcher_key_name(ir, key);
                    diagnostics.push(field_diagnostic(
                        ir,
                        fact.schema,
                        applicable[0],
                        Diagnostic::new(
                            DiagnosticCode::Cardinality,
                            severity(field.severity),
                            TextRange::empty(anchor.start()),
                            format!(
                                "`{name}` must occur at least {} {}",
                                min,
                                if min == 1 { "time" } else { "times" }
                            ),
                        ),
                    ));
                }
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
        let Some(fact) = hir
            .schema_facts()
            .iter()
            .filter(|fact| crate::support::contains(fact.range, value.range.start()))
            .min_by_key(|fact| fact.range.len())
        else {
            continue;
        };
        let Some(matcher) = ir.schema(fact.schema).items else {
            continue;
        };
        if !matcher_matches_with_state(snapshot, ir, matcher, &value.value, &facts, &fact.state) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                value.range,
                format!("expected {}", describe(ir, matcher)),
            ));
        }
    }
    diagnostics.extend(control_lints(ir, hir, &facts, cancellation)?);
    diagnostics.extend(callable_argument_diagnostics(snapshot, hir, cancellation)?);
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

fn quoted_schema_diagnostics(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
    session: &mut QuotedScriptSession<'_>,
    layers: &mut Vec<(text::TextSize, QuotedScript)>,
    out: &mut Vec<Diagnostic>,
) -> Result<(), Cancelled> {
    let ir = snapshot.ir();
    let facts = WorkspaceFacts { snapshot };
    let source = hir.syntax().source();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let Some(scalar) = property.scalar.as_ref().filter(|scalar| scalar.quoted) else {
            continue;
        };
        let Some(field_fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let mut schemas = Vec::new();
        for field_id in &field_fact.fields {
            match ir.field(*field_id).value {
                FieldValue::Quoted(schema) => schemas.push(schema),
                FieldValue::Scalar(matcher) => {
                    collect_quoted_matcher_schemas(ir, matcher, &mut schemas)
                }
                _ => {}
            }
        }
        schemas.sort_unstable();
        schemas.dedup();
        if schemas.is_empty() {
            continue;
        }
        let Some(raw) = source.get(
            usize::try_from(scalar.range.start()).unwrap_or(source.len())
                ..usize::try_from(scalar.range.end()).unwrap_or(source.len()),
        ) else {
            continue;
        };
        let script = match session.parse(raw, layers.len())? {
            QuotedScriptParse::Parsed(script) => script,
            QuotedScriptParse::Opaque | QuotedScriptParse::Limited(_) => continue,
        };
        let parent = hir
            .schema_facts()
            .iter()
            .filter(|fact| {
                fact.schema == field_fact.schema
                    && crate::support::contains(fact.range, property.key_range.start())
            })
            .min_by_key(|fact| fact.range.len());
        let Some(parent) = parent else {
            continue;
        };
        let state = parent.state.clone();
        for schema in schemas {
            cancellation.checkpoint()?;
            let fragment = hir::lower_ir_schema(
                std::sync::Arc::new(script.parsed().clone()),
                ir,
                schema,
                field_fact.subtypes.clone(),
                state.clone(),
                &facts,
            );
            layers.push((scalar.range.start(), script.clone()));
            let mut nested = schema_diagnostics(snapshot, &fragment, cancellation)?;
            for diagnostic in &mut nested {
                if let Some(mapped) = map_quoted_range(diagnostic.range, layers) {
                    diagnostic.range = mapped;
                }
            }
            out.extend(nested);
            quoted_schema_diagnostics(snapshot, &fragment, cancellation, session, layers, out)?;
            layers.pop();
        }
    }
    Ok(())
}

fn collect_quoted_matcher_schemas(ir: &RulesIr, matcher: MatcherId, out: &mut Vec<SchemaId>) {
    match ir.matcher(matcher) {
        Matcher::Quoted(schema) => out.push(*schema),
        Matcher::Union(alternatives) => {
            for alternative in alternatives {
                collect_quoted_matcher_schemas(ir, *alternative, out);
            }
        }
        _ => {}
    }
}

fn map_quoted_range(
    mut range: TextRange,
    layers: &[(text::TextSize, QuotedScript)],
) -> Option<TextRange> {
    for (token_start, script) in layers.iter().rev() {
        let relative = script.source_map().decoded_range(range)?;
        range = TextRange::new(
            token_start.checked_add(relative.start())?,
            token_start.checked_add(relative.end())?,
        )?;
    }
    Some(range)
}

/// Checks parameter bindings on blocks invoking a symbol backed by the IR's `Callable` trait.
/// The signature comes from HIR's definition/parameter facts; no legacy semantic rule table is
/// consulted on the IR path.
fn callable_argument_diagnostics(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let ir = snapshot.ir();
    let Some(callable) = ir.trait_by_name("Callable") else {
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
            callable_targets(ir, field.key, &mut type_ids);
            type_ids.into_iter().find_map(|type_id| {
                let info = ir.type_info(type_id);
                let implements = info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == callable)
                    || info.subtypes.iter().any(|subtype| {
                        field_fact.subtypes.contains(type_id, subtype.name)
                            && subtype
                                .trait_impls
                                .iter()
                                .any(|implementation| implementation.trait_id == callable)
                    });
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
        let Some(summary) =
            crate::semantic::dynamic_definition_summary(snapshot, &kind, &property.key)
        else {
            continue;
        };
        let parameter_matchers =
            callable_parameter_matchers(snapshot, &kind, &property.key, cancellation)?;
        let child_path_len = property.path.len() + 1;
        let arguments = hir
            .properties()
            .iter()
            .filter(|child| {
                child.path.len() == child_path_len
                    && child.path.starts_with(&property.path)
                    && property.range.start() <= child.range.start()
                    && child.range.end() <= property.range.end()
            })
            .collect::<Vec<_>>();
        let mut counts = BTreeMap::<String, u32>::new();
        for argument in &arguments {
            cancellation.checkpoint()?;
            if let (Some(scalar), Some(matchers)) = (
                argument.scalar.as_ref(),
                parameter_matchers.get(&argument.key.to_ascii_lowercase()),
            ) {
                let invalid = matchers
                    .iter()
                    .copied()
                    .filter(|matcher| {
                        !matcher_matches_in_snapshot(
                            snapshot,
                            ir,
                            *matcher,
                            &scalar.value,
                            &WorkspaceFacts { snapshot },
                        )
                    })
                    .collect::<Vec<_>>();
                if !invalid.is_empty() {
                    let expected = invalid
                        .iter()
                        .map(|matcher| describe(ir, *matcher))
                        .collect::<Vec<_>>();
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        DiagnosticCode::InvalidValue.severity(),
                        scalar.range,
                        format!("expected {}", expected.join(" and ")),
                    ));
                }
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
                        "unexpected parameter `{}` of scripted `{}`",
                        argument.key, summary.name
                    ),
                ));
            }
        }
        let missing = summary
            .parameters
            .iter()
            .filter(|parameter| {
                parameter.required
                    && !counts
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case(&parameter.name))
            })
            .map(|parameter| format!("`{}`", parameter.name))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                DiagnosticCode::Cardinality.severity(),
                property.key_range,
                format!(
                    "dynamic definition `{}` is missing required parameter(s): {}",
                    summary.name,
                    missing.join(", ")
                ),
            ));
        }
    }
    Ok(diagnostics)
}

fn callable_argument_key_ranges(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    cancellation: &CancellationToken,
) -> Result<std::collections::BTreeSet<TextRange>, Cancelled> {
    let ir = snapshot.ir();
    let Some(callable) = ir.trait_by_name("Callable") else {
        return Ok(Default::default());
    };
    let mut ranges = std::collections::BTreeSet::new();
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
            callable_targets(ir, ir.field(*field_id).key, &mut targets);
            targets.into_iter().find_map(|type_id| {
                let info = ir.type_info(type_id);
                let implements = info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == callable)
                    || info.subtypes.iter().any(|subtype| {
                        field_fact.subtypes.contains(type_id, subtype.name)
                            && subtype
                                .trait_impls
                                .iter()
                                .any(|implementation| implementation.trait_id == callable)
                    });
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
        }) || crate::semantic::dynamic_definition_summary(snapshot, &kind, &invocation.key)
            .is_none()
        {
            continue;
        }
        ranges.extend(
            hir.properties()
                .iter()
                .filter(|property| {
                    property.path.len() == invocation.path.len() + 1
                        && property.path.starts_with(&invocation.path)
                })
                .map(|property| property.key_range),
        );
    }
    Ok(ranges)
}

fn callable_targets(ir: &RulesIr, matcher: MatcherId, out: &mut Vec<TypeId>) {
    match ir.matcher(matcher) {
        Matcher::Ref(RefTarget::Type { type_id, .. }) | Matcher::Def { type_id, .. } => {
            out.push(*type_id)
        }
        Matcher::Union(alternatives) => {
            for alternative in alternatives {
                callable_targets(ir, *alternative, out);
            }
        }
        _ => {}
    }
}

/// Infers the IR scalar matcher constraints attached to Callable parameters by replaying the
/// definition template against its declared body schema. Runtime branching is conservatively
/// unioned; each distinct usage remains available to validate all reachable constraints.
pub(crate) fn callable_parameter_matchers(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<BTreeMap<String, Vec<MatcherId>>, Cancelled> {
    cancellation.checkpoint()?;
    let ir = snapshot.ir();
    let Some(summary) = crate::semantic::dynamic_definition_summary(snapshot, kind, name) else {
        return Ok(BTreeMap::new());
    };
    let Some(template) = summary.template.as_ref() else {
        return Ok(BTreeMap::new());
    };
    let Some(type_id) = ir.type_by_name(kind) else {
        return Ok(BTreeMap::new());
    };
    let Some(callable) = ir.trait_by_name("Callable") else {
        return Ok(BTreeMap::new());
    };
    let body_name = ir
        .type_info(type_id)
        .trait_impls
        .iter()
        .filter(|implementation| implementation.trait_id == callable)
        .find_map(|implementation| {
            implementation.arguments.iter().find_map(|(key, value)| {
                (ir.strings().resolve(*key) == "body").then(|| match value {
                    rules::ir::TraitArgument::Text(body) => ir.strings().resolve(*body).to_owned(),
                    rules::ir::TraitArgument::Binding(_) => String::new(),
                })
            })
        });
    let Some(body_name) = body_name.filter(|name| !name.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let Some(schema) = ir.schema_by_name(&body_name) else {
        return Ok(BTreeMap::new());
    };
    let facts = WorkspaceFacts { snapshot };
    let mut matchers = BTreeMap::new();
    let mut budget = 16_384;
    collect_template_matchers(
        ir,
        &facts,
        schema,
        &template.items,
        &mut matchers,
        cancellation,
        &mut budget,
    )?;
    for matchers in matchers.values_mut() {
        matchers.sort_unstable();
        matchers.dedup();
    }
    Ok(matchers)
}

#[derive(Default)]
struct TemplateScalars(BTreeMap<String, String>);

impl ScalarFields for TemplateScalars {
    fn scalar(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }
}

fn template_scalars(items: &[TemplateItem]) -> TemplateScalars {
    let mut result = TemplateScalars::default();
    for item in items {
        let TemplateItem::Property(property) = item else {
            continue;
        };
        let Some(key) = template_literal(&property.key) else {
            continue;
        };
        let TemplateValue::Scalar(value) = &property.value else {
            continue;
        };
        if let Some(value) = template_literal(value) {
            result.0.insert(key, value);
        }
    }
    result
}

fn template_literal(token: &TemplateToken) -> Option<String> {
    let mut value = String::new();
    for fragment in &token.fragments {
        match fragment {
            TemplateFragment::Literal(text) => value.push_str(text),
            TemplateFragment::Parameter { .. } => return None,
        }
    }
    Some(value)
}

fn template_parameters(token: &TemplateToken) -> Vec<String> {
    token
        .fragments
        .iter()
        .filter_map(|fragment| match fragment {
            TemplateFragment::Parameter { name, .. } => Some(name.to_ascii_lowercase()),
            TemplateFragment::Literal(_) => None,
        })
        .collect()
}

fn collect_template_matchers(
    ir: &RulesIr,
    facts: &impl SymbolFacts,
    schema: SchemaId,
    items: &[TemplateItem],
    out: &mut BTreeMap<String, Vec<MatcherId>>,
    cancellation: &CancellationToken,
    budget: &mut usize,
) -> Result<(), Cancelled> {
    let subtypes = ir.subtypes_of(schema, &template_scalars(items));
    for item in items {
        cancellation.checkpoint()?;
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        match item {
            TemplateItem::Conditional(conditional) => {
                collect_template_matchers(
                    ir,
                    facts,
                    schema,
                    &conditional.items,
                    out,
                    cancellation,
                    budget,
                )?;
            }
            TemplateItem::BareValue(token) => {
                let Some(matcher) = ir.schema(schema).items else {
                    continue;
                };
                for parameter in template_parameters(token) {
                    out.entry(parameter).or_default().push(matcher);
                }
            }
            TemplateItem::Property(property) => {
                let Some(key) = template_literal(&property.key) else {
                    continue;
                };
                let shape = match &property.value {
                    TemplateValue::Scalar(token) if token.quoted => Shape::Quoted,
                    TemplateValue::Scalar(_) => Shape::Scalar,
                    TemplateValue::Block { .. } => Shape::Block,
                };
                let mut fields = ir.lookup(schema, &key, shape).collect::<Vec<_>>();
                if shape == Shape::Quoted {
                    fields.extend(ir.lookup(schema, &key, Shape::Scalar));
                }
                fields.retain(|id| {
                    ir.gate_holds(ir.field(*id).gate, &subtypes)
                        && matcher_matches(ir, ir.field(*id).key, &key, facts)
                });
                for field_id in fields {
                    let field = ir.field(field_id);
                    for parameter in template_parameters(&property.key) {
                        out.entry(parameter).or_default().push(field.key);
                    }
                    match &property.value {
                        TemplateValue::Scalar(token) => {
                            let matcher = match field.value {
                                FieldValue::Scalar(matcher) => Some(matcher),
                                _ => None,
                            };
                            if let Some(matcher) = matcher {
                                for parameter in template_parameters(token) {
                                    out.entry(parameter).or_default().push(matcher);
                                }
                            }
                        }
                        TemplateValue::Block { items, .. } => {
                            if let Some(child) = ir.child(field_id, schema) {
                                collect_template_matchers(
                                    ir,
                                    facts,
                                    child,
                                    items,
                                    out,
                                    cancellation,
                                    budget,
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

trait HirSchemaLookup {
    fn schema_facts_for_range<'a>(
        &'a self,
        hir: &'a HirFile,
        schema: SchemaId,
        key: TextRange,
    ) -> Option<&'a hir::SchemaFact>;
}

impl HirSchemaLookup for RulesIr {
    fn schema_facts_for_range<'a>(
        &'a self,
        hir: &'a HirFile,
        schema: SchemaId,
        key: TextRange,
    ) -> Option<&'a hir::SchemaFact> {
        hir.schema_facts()
            .iter()
            .filter(|fact| {
                fact.schema == schema && crate::support::contains(fact.range, key.start())
            })
            .min_by_key(|fact| fact.range.len())
    }
}

fn scope_allows(ir: &RulesIr, current: &ScopeValue, expected: &[rules::ir::Symbol]) -> bool {
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

fn control_lints(
    ir: &RulesIr,
    hir: &HirFile,
    facts: &impl SymbolFacts,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let mut diagnostics = Vec::new();
    for property in hir.properties() {
        cancellation.checkpoint()?;
        let Some(fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(field) = fact.fields.first().map(|field| ir.field(*field)) else {
            continue;
        };
        let Some(control) = &field.control else {
            continue;
        };
        match control.kind {
            ControlKind::Logic => {
                let children = hir
                    .properties()
                    .iter()
                    .filter(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                    })
                    .collect::<Vec<_>>();
                let op = control.op.map(|op| ir.strings().resolve(op));
                let invalid_count = if op.is_some_and(|op| op.eq_ignore_ascii_case("not")) {
                    children.len() != 1
                } else {
                    children.is_empty()
                };
                if invalid_count {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::LogicalContainer,
                        DiagnosticCode::LogicalContainer.severity(),
                        property.range,
                        if op.is_some_and(|op| op.eq_ignore_ascii_case("not")) {
                            "NOT requires exactly one condition".into()
                        } else {
                            "empty logic container".into()
                        },
                    ));
                }
                let constants = children
                    .iter()
                    .filter_map(|child| {
                        child.scalar.as_ref().and_then(|scalar| {
                            let value = scalar.value.to_ascii_lowercase();
                            (value == "yes" || value == "no").then_some(value == "yes")
                        })
                    })
                    .collect::<Vec<_>>();
                let constant = if op.is_some_and(|op| op.eq_ignore_ascii_case("and"))
                    && constants.contains(&false)
                {
                    Some(false)
                } else if op.is_some_and(|op| op.eq_ignore_ascii_case("or"))
                    && constants.contains(&true)
                {
                    Some(true)
                } else if op.is_some_and(|op| op.eq_ignore_ascii_case("not"))
                    && constants.len() == 1
                {
                    Some(!constants[0])
                } else {
                    None
                };
                if let Some(value) = constant {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::ConstantCondition,
                        DiagnosticCode::ConstantCondition.severity(),
                        property.range,
                        format!(
                            "this logic container is always {}",
                            if value { "true" } else { "false" }
                        ),
                    ));
                }
            }
            ControlKind::Branch | ControlKind::BranchContinue => {
                if let Some(guard) = control.guard {
                    let guard_name = ir.strings().resolve(guard);
                    if !hir.properties().iter().any(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                            && child.key.eq_ignore_ascii_case(guard_name)
                    }) {
                        diagnostics.push(Diagnostic::new(
                            DiagnosticCode::MissingLimit,
                            DiagnosticCode::MissingLimit.severity(),
                            property.key_range,
                            format!("`{}` requires a `{guard_name}` block", property.key),
                        ));
                    }
                }
                if control.kind == ControlKind::BranchContinue {
                    let parent_path = property
                        .path
                        .get(..property.path.len().saturating_sub(1))
                        .unwrap_or(&[]);
                    let container = hir
                        .schema_at(property.key_range.start())
                        .map(|fact| fact.range);
                    let mut previous = hir
                        .properties()
                        .iter()
                        .filter(|sibling| {
                            sibling.path.len() == property.path.len()
                                && sibling.path.get(..sibling.path.len().saturating_sub(1))
                                    == Some(parent_path)
                                && sibling.range.end() <= property.range.start()
                                && container.is_none_or(|container| {
                                    container.start() <= sibling.range.start()
                                        && sibling.range.end() <= container.end()
                                })
                        })
                        .collect::<Vec<_>>();
                    previous.sort_by_key(|sibling| std::cmp::Reverse(sibling.range.start()));
                    let mut attached = false;
                    for sibling in previous {
                        let previous_control = hir
                            .field_fact_at(sibling.key_range)
                            .and_then(|fact| fact.fields.first())
                            .and_then(|id| ir.field(*id).control.as_ref());
                        match previous_control {
                            Some(previous)
                                if matches!(
                                    previous.kind,
                                    ControlKind::Branch | ControlKind::BranchContinue
                                ) =>
                            {
                                if !previous.chain.is_empty() {
                                    attached = previous.chain.iter().any(|key| {
                                        ir.strings()
                                            .resolve(*key)
                                            .eq_ignore_ascii_case(&property.key)
                                    });
                                    break;
                                }
                                if previous.kind == ControlKind::Branch {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                    if !attached {
                        diagnostics.push(Diagnostic::new(
                            DiagnosticCode::OrphanElse,
                            DiagnosticCode::OrphanElse.severity(),
                            property.key_range,
                            format!("{} has no preceding branch in its chain", property.key),
                        ));
                    }
                }
            }
            ControlKind::Switch => {
                if let Some(on) = control.on {
                    let selector = hir
                        .properties()
                        .iter()
                        .find(|child| {
                            child.path.len() == property.path.len() + 1
                                && child.path.starts_with(&property.path)
                                && property.range.start() <= child.range.start()
                                && child.range.end() <= property.range.end()
                                && child.key.eq_ignore_ascii_case(ir.strings().resolve(on))
                        })
                        .and_then(|child| child.scalar.as_ref());
                    if let Some(selector) = selector
                        && let Some(trigger_schema) = ir.schema_by_name("trigger")
                    {
                        let matchers = ir
                            .lookup(trigger_schema, &selector.value, Shape::Scalar)
                            .filter_map(|id| {
                                let field = ir.field(id);
                                if !ir.gate_holds(field.gate, &fact.subtypes)
                                    || !matcher_matches(ir, field.key, &selector.value, facts)
                                {
                                    return None;
                                }
                                match field.value {
                                    FieldValue::Scalar(matcher) => Some(matcher),
                                    _ => None,
                                }
                            })
                            .collect::<Vec<_>>();
                        if matchers.is_empty() {
                            diagnostics.push(Diagnostic::new(
                                DiagnosticCode::InvalidValue,
                                DiagnosticCode::InvalidValue.severity(),
                                selector.range,
                                format!("unknown scalar trigger `{}`", selector.value),
                            ));
                        } else {
                            for child in hir.properties().iter().filter(|child| {
                                child.path.len() == property.path.len() + 1
                                    && child.path.starts_with(&property.path)
                                    && property.range.start() <= child.range.start()
                                    && child.range.end() <= property.range.end()
                                    && child.scalar.is_none()
                            }) {
                                if !matchers
                                    .iter()
                                    .any(|matcher| ir.scalar_matches(*matcher, &child.key, facts))
                                {
                                    diagnostics.push(Diagnostic::new(
                                        DiagnosticCode::InvalidValue,
                                        DiagnosticCode::InvalidValue.severity(),
                                        child.key_range,
                                        format!(
                                            "expected {} for `{}` branch",
                                            matchers
                                                .iter()
                                                .map(|matcher| describe(ir, *matcher))
                                                .collect::<Vec<_>>()
                                                .join(" or "),
                                            selector.value
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            ControlKind::Guard | ControlKind::DisplayOnly => {
                let has_children = hir.properties().iter().any(|child| {
                    child.path.len() == property.path.len() + 1
                        && child.path.starts_with(&property.path)
                        && property.range.start() <= child.range.start()
                        && child.range.end() <= property.range.end()
                });
                if !has_children {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::EmptyBlock,
                        DiagnosticCode::EmptyBlock.severity(),
                        property.range,
                        format!("{} has an empty body", property.key),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{AnalysisHost, DocumentId};
    use text::TextSize;

    #[test]
    fn production_diagnostics_and_completion_use_first_party_ir_facts() {
        let ir = game::eu4::first_party_ir().expect("first-party IR");
        let rules = game::eu4::first_party_rules().expect("first-party rules");
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

        let callable_definition =
            DocumentId::new("file:///tmp/common/scripted_effects/phase4-callable.txt");
        host.open_document(
            callable_definition,
            1,
            "phase4_callable = { add_prestige = $AMOUNT$ }".to_owned(),
            None,
        )
        .expect("open Callable definition");
        let call_source = "country_event = { id = phase4.call immediate = { phase4_callable = {  } } option = { name = phase4.option } }";
        let call_id = DocumentId::new("file:///tmp/common/events/phase4-callable-site.txt");
        host.open_document(call_id.clone(), 2, call_source.to_owned(), None)
            .expect("open Callable invocation");
        let call_diagnostics = crate::diagnostics::diagnostics(&host.snapshot(), &call_id);
        assert!(
            call_diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DiagnosticCode::Cardinality
                    && diagnostic.message.contains("AMOUNT")
            }),
            "required Callable parameter should be diagnosed: {call_diagnostics:#?}"
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
