//! Bounded finite string relations. Repeated occurrences share one binding.
//! The same rows can be joined across usage sites; a witness is never chosen independently.
use crate::analysis::{Analysis, AnalysisCoverage, AnalysisLimit};
use rules::replacement::TemplateFragment;
use std::collections::{BTreeMap, BTreeSet};

/// One consistent assignment of parameter names to strings.
pub type Witness = BTreeMap<String, String>;

/// A query-wide search bound, shared by inversions and relation joins.
pub struct SearchBudget {
    remaining: usize,
    /// Coverage accumulated while searching.
    pub coverage: AnalysisCoverage,
}
impl SearchBudget {
    /// Starts a finite search with an explicit work allowance.
    pub fn new(steps: usize) -> Self {
        Self {
            remaining: steps,
            coverage: AnalysisCoverage::default(),
        }
    }
    /// Charges work before allocation; a stopped branch is observable.
    pub fn step<E>(&mut self, checkpoint: &mut dyn FnMut() -> Result<(), E>) -> Result<bool, E> {
        checkpoint()?;
        if self.remaining == 0 {
            self.coverage.limits.insert(AnalysisLimit::Nodes);
            Ok(false)
        } else {
            self.remaining -= 1;
            Ok(true)
        }
    }
}

/// Inverts a finite candidate string without guessing independent values for repeated slots.
/// UTF-8 boundaries are preserved. Empty assignments are allowed here and checked by consumers.
pub fn inverse<E>(
    fragments: &[TemplateFragment],
    value: &str,
    budget: &mut SearchBudget,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Vec<Witness>, E> {
    let names = fragments
        .iter()
        .filter_map(|part| match part {
            TemplateFragment::Parameter { name, .. } => Some(name.to_ascii_lowercase()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    // One unknown has a unique byte length even when it occurs many times.
    if names.len() == 1 {
        if !budget.step(checkpoint)? {
            return Ok(Vec::new());
        }
        let literal_bytes = fragments
            .iter()
            .filter_map(|part| match part {
                TemplateFragment::Literal(text) => Some(text.len()),
                _ => None,
            })
            .sum::<usize>();
        let count = fragments
            .iter()
            .filter(|part| matches!(part, TemplateFragment::Parameter { .. }))
            .count();
        let Some(bytes) = value.len().checked_sub(literal_bytes) else {
            return Ok(Vec::new());
        };
        if bytes % count != 0 {
            return Ok(Vec::new());
        }
        let width = bytes / count;
        let mut offset = 0;
        let mut bound: Option<&str> = None;
        for part in fragments {
            let len = match part {
                TemplateFragment::Literal(text) => text.len(),
                _ => width,
            };
            let Some(piece) = value.get(offset..offset + len) else {
                return Ok(Vec::new());
            };
            let expected = match part {
                TemplateFragment::Literal(text) => text.as_str(),
                _ => *bound.get_or_insert(piece),
            };
            if !piece.eq_ignore_ascii_case(expected) {
                return Ok(Vec::new());
            }
            offset += len;
        }
        return Ok(vec![Witness::from([(
            names.into_iter().next().expect("one name"),
            bound.unwrap_or("").to_owned(),
        )])]);
    }
    let mut rows = BTreeSet::new();
    let mut pending = vec![(0usize, 0usize, Witness::new())];
    while let Some((index, offset, bindings)) = pending.pop() {
        if !budget.step(checkpoint)? {
            break;
        }
        let Some(part) = fragments.get(index) else {
            if offset == value.len() {
                rows.insert(bindings);
            }
            continue;
        };
        match part {
            TemplateFragment::Literal(text) => {
                if value
                    .get(offset..offset.saturating_add(text.len()))
                    .is_some_and(|head| head.eq_ignore_ascii_case(text))
                {
                    pending.push((index + 1, offset + text.len(), bindings));
                }
            }
            TemplateFragment::Parameter { name, .. } => {
                let name = name.to_ascii_lowercase();
                if let Some(bound) = bindings.get(&name) {
                    if value
                        .get(offset..offset.saturating_add(bound.len()))
                        .is_some_and(|head| head.eq_ignore_ascii_case(bound))
                    {
                        pending.push((index + 1, offset + bound.len(), bindings));
                    }
                } else if let Some(tail) = value.get(offset..) {
                    for end in tail
                        .char_indices()
                        .map(|(i, _)| i)
                        .chain(std::iter::once(tail.len()))
                    {
                        if !budget.step(checkpoint)? {
                            break;
                        }
                        let mut next = bindings.clone();
                        next.insert(name.clone(), tail[..end].to_owned());
                        pending.push((index + 1, offset + end, next));
                    }
                }
            }
        }
    }
    Ok(rows.into_iter().collect())
}

/// Natural join: all usages must agree on every common parameter.
pub fn join<E>(
    left: &[Witness],
    right: &[Witness],
    budget: &mut SearchBudget,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Vec<Witness>, E> {
    let mut joined = BTreeSet::new();
    'outer: for a in left {
        for b in right {
            if !budget.step(checkpoint)? {
                break 'outer;
            }
            if a.iter().all(|(name, value)| {
                b.get(name)
                    .is_none_or(|other| other.eq_ignore_ascii_case(value))
            }) {
                let mut row = a.clone();
                row.extend(b.clone());
                joined.insert(row);
            }
        }
    }
    Ok(joined.into_iter().collect())
}

/// Packages the accumulated relation and its exact search coverage.
pub fn result(rows: Vec<Witness>, budget: SearchBudget) -> Analysis<Vec<Witness>> {
    Analysis {
        value: rows,
        coverage: budget.coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token(parts: &[&str]) -> Vec<TemplateFragment> {
        parts
            .iter()
            .map(|part| {
                part.strip_prefix('$').map_or_else(
                    || TemplateFragment::Literal((*part).into()),
                    |name| TemplateFragment::Parameter {
                        name: name.into(),
                        range: text::TextRange::empty(0),
                    },
                )
            })
            .collect()
    }
    #[test]
    fn repeated_slot_inverse_matches_exhaustive_finite_domain() {
        let parts = token(&["pre_", "$p", "_", "$p", "_tail"]);
        for value in [
            "pre_a_a_tail",
            "pre_a_b_tail",
            "pre_界_界_tail",
            "pre___tail",
        ] {
            let mut budget = SearchBudget::new(1000);
            let actual =
                inverse::<std::convert::Infallible>(&parts, value, &mut budget, &mut || Ok(()))
                    .unwrap();
            let expected = ["", "a", "b", "界"]
                .into_iter()
                .filter(|p| format!("pre_{p}_{p}_tail") == value)
                .map(|p| Witness::from([("p".into(), p.into())]))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            assert!(budget.coverage.is_known());
        }
    }
    #[test]
    fn multiple_sites_share_one_witness_and_budget_is_explicit() {
        let mut budget = SearchBudget::new(10000);
        let first = inverse::<std::convert::Infallible>(
            &token(&["$p", "-", "$q"]),
            "a-b",
            &mut budget,
            &mut || Ok(()),
        )
        .unwrap();
        let second = inverse::<std::convert::Infallible>(
            &token(&["$p", "-", "$q"]),
            "a-c",
            &mut budget,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(
            join::<std::convert::Infallible>(&first, &second, &mut budget, &mut || Ok(()))
                .unwrap()
                .is_empty()
        );
        let mut budget = SearchBudget::new(0);
        assert!(
            inverse::<std::convert::Infallible>(&token(&["$p"]), "a", &mut budget, &mut || Ok(()))
                .unwrap()
                .is_empty()
        );
        assert!(!budget.coverage.is_complete());
    }
}
