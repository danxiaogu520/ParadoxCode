//! Bounded, case-insensitive did-you-mean suggestions.
//!
//! Suggestions are computed only on an already-invalid value.  The bounded two-row
//! Levenshtein scan keeps the error path cheap while avoiding noisy fixes for distant or
//! ambiguous candidates.

/// Maximum edit distance accepted for a suggestion.
const MAX_DISTANCE: usize = 2;

/// Very short candidates are too close to unrelated text to make useful suggestions.
const MIN_CANDIDATE_LEN: usize = 3;

/// Returns the ASCII case-insensitive Levenshtein distance when it is within `max`.
pub(crate) fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect();
    let b: Vec<char> = b
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect();
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > max {
        return None;
    }

    let mut previous: Vec<usize> = (0..=m).collect();
    let mut current = vec![0; m + 1];
    for i in 1..=n {
        current[0] = i;
        let mut row_min = current[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            row_min = row_min.min(current[j]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    Some(previous[m]).filter(|distance| *distance <= max)
}

/// Returns the unique closest candidate within [`MAX_DISTANCE`].
///
/// Ties at the minimal distance deliberately produce no suggestion: an automatic edit must not
/// choose between equally plausible enum members. Duplicate spellings are treated as one
/// candidate, which accommodates overloaded rule rows.
pub(crate) fn best_suggestion<'a, I>(key: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&'a str, usize)> = None;
    let mut tied = false;
    for candidate in candidates {
        if candidate.chars().count() < MIN_CANDIDATE_LEN {
            continue;
        }
        let Some(distance) = bounded_distance(key, candidate, MAX_DISTANCE) else {
            continue;
        };
        match best {
            Some((_, best_distance)) if distance < best_distance => {
                best = Some((candidate, distance));
                tied = false;
            }
            Some((best_candidate, best_distance))
                if distance == best_distance && !candidate.eq_ignore_ascii_case(best_candidate) =>
            {
                tied = true;
            }
            Some(_) => {}
            None => best = Some((candidate, distance)),
        }
    }
    match best {
        Some((candidate, _)) if !tied => Some(candidate),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_distance_handles_common_edits() {
        assert_eq!(bounded_distance("name", "name", 2), Some(0));
        assert_eq!(bounded_distance("naem", "name", 2), Some(2));
        assert_eq!(bounded_distance("cont", "count", 2), Some(1));
        assert_eq!(bounded_distance("namee", "name", 2), Some(1));
    }

    #[test]
    fn bounded_distance_is_case_insensitive_and_bounded() {
        assert_eq!(bounded_distance("NAME", "name", 2), Some(0));
        assert_eq!(bounded_distance("xyzzy", "name", 2), None);
        assert_eq!(bounded_distance("count", "co", 2), None);
    }

    #[test]
    fn best_suggestion_requires_a_unique_close_candidate() {
        assert_eq!(best_suggestion("naem", ["name", "count"]), Some("name"));
        assert_eq!(best_suggestion("rat", ["cat", "bat"]), None);
        assert_eq!(best_suggestion("ba", ["ab"]), None);
        assert_eq!(best_suggestion("cont", ["count", "count"]), Some("count"));
    }
}
