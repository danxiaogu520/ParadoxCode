//! Bounded, cancellable Pattern search shared by rule and script consumers.
use crate::ir::{Matcher, MatcherId, PatternPart, RulesIr};
use crate::query::{FieldQuery, NoQueryContext, QueryContext, QueryProjection, QueryState};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchLimit {
    Work,
    States,
    Depth,
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchLimits {
    pub work: usize,
    pub states: usize,
    pub depth: usize,
}
impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            work: 1_048_576,
            states: 8192,
            depth: 64,
        }
    }
}

/// One goal's shared budget, including nested matchers and failed alternatives.
pub struct SearchBudget<'a> {
    remaining: SearchLimits,
    depth: usize,
    checkpoint: &'a mut dyn FnMut() -> bool,
    pub limit: Option<SearchLimit>,
}
impl<'a> SearchBudget<'a> {
    pub fn new(limits: SearchLimits, checkpoint: &'a mut dyn FnMut() -> bool) -> Self {
        Self {
            remaining: limits,
            depth: 0,
            checkpoint,
            limit: None,
        }
    }
    fn charge(&mut self, work: usize, states: usize) -> bool {
        if self.limit.is_some() {
            return false;
        }
        self.limit = if (self.checkpoint)() {
            Some(SearchLimit::Cancelled)
        } else if work > self.remaining.work {
            Some(SearchLimit::Work)
        } else if states > self.remaining.states {
            Some(SearchLimit::States)
        } else {
            None
        };
        if self.limit.is_some() {
            return false;
        }
        self.remaining.work -= work;
        self.remaining.states -= states;
        true
    }
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    /// None means either unresolved hole evidence or unfinished search.
    pub matched: Option<bool>,
    /// A proved accepting path's exact hole slices. Empty for an unknown result.
    pub holes: Vec<(MatcherId, usize, usize)>,
}

/// Evaluates unions and nested Patterns with the same budget. Primitive semantics
/// remain owned by the caller (scope state, definition policy and workspace facts).
pub fn evaluate(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    budget: &mut SearchBudget<'_>,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> Option<bool> {
    evaluate_with_context(ir, matcher, value, budget, &NoQueryContext, primitive)
}

/// Evaluates dependent queries with the SAME depth/work/state/cancellation
/// budget as unions, patterns and source-key dispatch. Primitive semantics stay
/// with the caller, including scope, definition and workspace interpretation.
pub fn evaluate_with_context(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> Option<bool> {
    evaluate_call_key(ir, matcher, value, budget, context, false, primitive)
}

/// The same scalar search, optionally proving empty-argument invocation for
/// direct Template Ref/Union keys. Pattern holes and query projections are names,
/// not invocation targets; their own authored query constraints still apply.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_call_key(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    no_args: bool,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> Option<bool> {
    if no_args {
        let matched = evaluate_call_key(ir, matcher, value, budget, context, false, primitive);
        if matched != Some(true) || !context.check_call_arguments() {
            return matched;
        }
        let target = crate::template::template_key_type(ir, matcher, &mut || !budget.charge(1, 0));
        if budget.limit.is_some() {
            return None;
        }
        let Some(type_id) = target else {
            return Some(true);
        };
        let result =
            context.template_accepts_no_arguments(type_id, value, &mut || !budget.charge(1, 0));
        return if budget.limit.is_some() { None } else { result };
    }
    if !budget.charge(1, 0) {
        return None;
    }
    if budget.depth >= budget.remaining.depth {
        budget.limit = Some(SearchLimit::Depth);
        return None;
    }
    budget.depth += 1;
    let result = match ir.matcher(matcher) {
        Matcher::Union(items) => {
            let mut result = Some(false);
            for item in items.iter() {
                match evaluate_with_context(ir, *item, value, budget, context, primitive) {
                    Some(true) => {
                        result = Some(true);
                        break;
                    }
                    None => result = None,
                    Some(false) => {}
                }
                if budget.limit.is_some() {
                    break;
                }
            }
            result
        }
        Matcher::Pattern(parts) => {
            search_with_context(ir, parts, value, budget, context, primitive).matched
        }
        Matcher::Query(query) => evaluate_query(ir, query, value, budget, context, primitive),
        _ => {
            if budget.charge(value.len(), 0) {
                primitive(matcher, value)
            } else {
                None
            }
        }
    };
    budget.depth -= 1;
    result
}

pub(crate) fn evaluate_query(
    ir: &RulesIr,
    query: &FieldQuery,
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> Option<bool> {
    if !budget.charge(
        ir.schema(query.schema).exact.len() + ir.schema(query.schema).patterns.len(),
        0,
    ) {
        return None;
    }
    let mut key_matches = |id, text: &str, no_args| {
        evaluate_call_key(ir, id, text, budget, context, no_args, primitive)
    };
    let resolution = if query.projection == QueryProjection::Keys && query.selector.is_none() {
        ir.query_selected_fields_with(query, value, &mut key_matches)
    } else {
        ir.query_fields_with(query, context, &mut key_matches)
    };
    match resolution.state {
        QueryState::Deferred => return None,
        QueryState::Missing | QueryState::Duplicate | QueryState::Invalid => return Some(false),
        QueryState::Resolved => {}
    }
    let mut result = Some(false);
    for field in resolution.fields {
        if let Some(projected) = ir.query_projected_matcher(query, field) {
            match evaluate_with_context(ir, projected, value, budget, context, primitive) {
                Some(true) => return Some(true),
                None => result = None,
                Some(false) => {}
            }
        }
        if budget.limit.is_some() {
            break;
        }
    }
    result
}

#[derive(Clone, Copy)]
enum Task {
    At {
        part: usize,
        offset: usize,
        route: Option<usize>,
        unknown: bool,
        different: bool,
    },
    Hole {
        part: usize,
        start: usize,
        end: usize,
        route: Option<usize>,
        unknown: bool,
        different: bool,
    },
}

/// Lazy explicit search: a hole schedules one split at a time, so a large input
/// cannot allocate all candidate ends before a checkpoint or an accepting proof.
pub fn search(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    budget: &mut SearchBudget<'_>,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> SearchResult {
    search_with_context(ir, parts, value, budget, &NoQueryContext, primitive)
}

/// Pattern search retaining a physical sibling context for query holes.
pub fn search_with_context(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> SearchResult {
    search_except(ir, parts, value, budget, context, primitive, None)
}

/// A source slice is editable only when all accepting paths agree on its holes.
pub fn unique_search(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    budget: &mut SearchBudget<'_>,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> SearchResult {
    unique_search_with_context(ir, parts, value, budget, &NoQueryContext, primitive)
}

/// Unique accepting-hole proof retaining the same query context in both searches.
pub fn unique_search_with_context(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
) -> SearchResult {
    let first = search_with_context(ir, parts, value, budget, context, primitive);
    if first.matched != Some(true) {
        return first;
    }
    let other = search_except(
        ir,
        parts,
        value,
        budget,
        context,
        primitive,
        Some(&first.holes),
    );
    if other.matched == Some(false) {
        first
    } else {
        SearchResult {
            matched: None,
            holes: Vec::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn search_except(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    budget: &mut SearchBudget<'_>,
    context: &impl QueryContext,
    primitive: &mut impl FnMut(MatcherId, &str) -> Option<bool>,
    excluded: Option<&[(MatcherId, usize, usize)]>,
) -> SearchResult {
    if !budget.charge(parts.len(), parts.len()) {
        return SearchResult {
            matched: None,
            holes: Vec::new(),
        };
    }
    let mut count = 0;
    let ordinals = parts
        .iter()
        .map(|part| {
            let ordinal = count;
            if matches!(part, PatternPart::Hole(_)) {
                count += 1;
            }
            ordinal
        })
        .collect::<Vec<_>>();
    let mut pending = vec![Task::At {
        part: 0,
        offset: 0,
        route: None,
        unknown: false,
        different: false,
    }];
    let mut seen = BTreeSet::new();
    let mut routes = Vec::new();
    let mut unresolved = false;
    while let Some(task) = pending.pop() {
        if !budget.charge(1, 1) {
            break;
        }
        match task {
            Task::At {
                part,
                offset,
                route,
                unknown,
                different,
            } => {
                if !seen.insert((part, offset, unknown, different)) {
                    continue;
                }
                match parts.get(part) {
                    None if offset == value.len() => {
                        if excluded.is_some() && !different {
                            continue;
                        }
                        if unknown {
                            unresolved = true;
                            continue;
                        }
                        let mut holes = Vec::new();
                        let mut current = route;
                        while let Some(id) = current {
                            let (parent, hole): (Option<usize>, (MatcherId, usize, usize)) =
                                routes[id];
                            holes.push(hole);
                            current = parent;
                        }
                        holes.reverse();
                        return SearchResult {
                            matched: Some(true),
                            holes,
                        };
                    }
                    Some(PatternPart::Text(text)) => {
                        let text = ir.strings.resolve(*text);
                        if !budget.charge(text.len(), 0) {
                            break;
                        }
                        if value
                            .get(offset..)
                            .and_then(|tail| tail.get(..text.len()))
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(text))
                        {
                            pending.push(Task::At {
                                part: part + 1,
                                offset: offset + text.len(),
                                route,
                                unknown,
                                different,
                            });
                        }
                    }
                    Some(PatternPart::Hole(_)) => {
                        if let Some(ch) = value.get(offset..).and_then(|tail| tail.chars().next()) {
                            pending.push(Task::Hole {
                                part,
                                start: offset,
                                end: if part + 1 == parts.len() {
                                    value.len()
                                } else {
                                    offset + ch.len_utf8()
                                },
                                route,
                                unknown,
                                different,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Task::Hole {
                part,
                start,
                end,
                route,
                unknown,
                different,
            } => {
                if let Some(ch) = value.get(end..).and_then(|tail| tail.chars().next()) {
                    pending.push(Task::Hole {
                        part,
                        start,
                        end: end + ch.len_utf8(),
                        route,
                        unknown,
                        different,
                    });
                }
                let PatternPart::Hole(matcher) = parts[part] else {
                    unreachable!()
                };
                let matched = evaluate_with_context(
                    ir,
                    matcher,
                    &value[start..end],
                    budget,
                    context,
                    primitive,
                );
                if matched != Some(false) {
                    if !budget.charge(1, 1) {
                        break;
                    }
                    let next = routes.len();
                    routes.push((route, (matcher, start, end)));
                    pending.push(Task::At {
                        part: part + 1,
                        offset: end,
                        route: Some(next),
                        unknown: unknown || matched.is_none(),
                        different: different
                            || excluded.is_some_and(|holes| {
                                holes.get(ordinals[part]) != Some(&(matcher, start, end))
                            }),
                    });
                }
            }
        }
    }
    SearchResult {
        matched: if unresolved || budget.limit.is_some() {
            None
        } else {
            Some(false)
        },
        holes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_splits_find_a_semantic_witness_and_keep_utf8_ranges() {
        let mut ir = RulesIr::empty();
        let scalar = MatcherId(ir.matchers.len() as u32);
        ir.matchers.push(Matcher::Scalar);
        let text = ir.strings.intern_verbatim("_");
        let parts = [
            PatternPart::Hole(scalar),
            PatternPart::Text(text),
            PatternPart::Hole(scalar),
        ];
        let mut no_cancel = || false;
        let mut budget = SearchBudget::new(Default::default(), &mut no_cancel);
        let found = search(&ir, &parts, "é_a_b", &mut budget, &mut |_, value| {
            Some(value != "é")
        });
        assert_eq!(found.matched, Some(true));
        assert_eq!(found.holes, vec![(scalar, 0, 4), (scalar, 5, 6)]);
    }

    #[test]
    fn limits_and_cancellation_are_unknown_and_a_larger_budget_can_finish() {
        let mut ir = RulesIr::empty();
        ir.matchers.push(Matcher::Scalar);
        let hole = PatternPart::Hole(MatcherId(0));
        let text = ir.strings.intern_verbatim("!");
        let parts = [hole, hole, PatternPart::Text(text)];
        let value = "a".repeat(2000);
        let mut no_cancel = || false;
        let mut budget = SearchBudget::new(
            SearchLimits {
                work: 20,
                ..Default::default()
            },
            &mut no_cancel,
        );
        assert_eq!(
            search(&ir, &parts, &value, &mut budget, &mut |_, _| Some(true)).matched,
            None
        );
        assert_eq!(budget.limit, Some(SearchLimit::Work));
        let mut calls = 0;
        let mut cancel = || {
            calls += 1;
            calls >= 5
        };
        let mut budget = SearchBudget::new(Default::default(), &mut cancel);
        assert_eq!(
            search(&ir, &parts, &value, &mut budget, &mut |_, _| Some(true)).matched,
            None
        );
        assert_eq!(budget.limit, Some(SearchLimit::Cancelled));
        let mut no_cancel = || false;
        let mut budget = SearchBudget::new(Default::default(), &mut no_cancel);
        assert_eq!(
            search(&ir, &parts, "ab!", &mut budget, &mut |_, _| Some(true)).matched,
            Some(true)
        );
    }
}
