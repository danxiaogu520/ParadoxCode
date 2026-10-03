//! Bounded, case-insensitive did-you-mean suggestions.
//!
//! Suggestions are computed only on an already-invalid value.  The bounded two-row
//! Levenshtein scan keeps the error path cheap while avoiding noisy fixes for distant or
//! ambiguous candidates.

/// Maximum edit distance accepted for a suggestion.
const MAX_DISTANCE: usize = 2;

/// Very short candidates are too close to unrelated text to make useful suggestions.
const MIN_CANDIDATE_LEN: usize = 3;

/// Reuses the query and edit-distance rows across a candidate set. Only cells
/// within `max` of the diagonal can contribute to an accepted distance.
struct DistanceWorkspace {
    query: Vec<char>,
    candidate: Vec<char>,
    previous: Vec<usize>,
    current: Vec<usize>,
}

impl DistanceWorkspace {
    fn new(query: &str) -> Self {
        Self {
            query: query.chars().map(|ch| ch.to_ascii_lowercase()).collect(),
            candidate: Vec::new(),
            previous: Vec::new(),
            current: Vec::new(),
        }
    }

    fn distance(&mut self, candidate: &str, max: usize) -> Option<usize> {
        self.candidate.clear();
        self.candidate
            .extend(candidate.chars().map(|ch| ch.to_ascii_lowercase()));
        let (n, m) = (self.query.len(), self.candidate.len());
        if n.abs_diff(m) > max {
            return None;
        }
        if n == 0 {
            return Some(m);
        }
        let max = max.min(n.max(m));
        let outside = max + 1;
        self.previous.resize(m + 1, outside);
        self.previous.fill(outside);
        for (j, cell) in self.previous.iter_mut().take(max + 1).enumerate() {
            *cell = j;
        }
        self.current.resize(m + 1, outside);
        for i in 1..=n {
            self.current[0] = i.min(outside);
            let first = i.saturating_sub(max).max(1);
            let last = i.saturating_add(max).min(m);
            if first > 1 {
                self.current[first - 1] = outside;
            }
            if last < m {
                self.current[last + 1] = outside;
            }
            let mut row_min = self.current[0];
            for j in first..=last {
                let cost = usize::from(self.query[i - 1] != self.candidate[j - 1]);
                self.current[j] = (self.previous[j] + 1)
                    .min(self.current[j - 1] + 1)
                    .min(self.previous[j - 1] + cost);
                row_min = row_min.min(self.current[j]);
            }
            if row_min > max {
                return None;
            }
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        Some(self.previous[m]).filter(|distance| *distance <= max)
    }
}

#[cfg(test)]
fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    DistanceWorkspace::new(a).distance(b, max)
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
    // Candidate sets such as the workspace localisation index reach tens of
    // thousands of entries; the length gate keeps the error path allocation-free
    // for everything that cannot possibly fall within `MAX_DISTANCE`.
    let mut workspace = DistanceWorkspace::new(key);
    let key_len = workspace.query.len();
    let mut best: Option<(&'a str, usize)> = None;
    let mut tied = false;
    for candidate in candidates {
        let candidate_len = candidate.chars().count();
        if candidate_len.abs_diff(key_len) > MAX_DISTANCE {
            continue;
        }
        if candidate_len < MIN_CANDIDATE_LEN {
            continue;
        }
        let Some(distance) = workspace.distance(candidate, MAX_DISTANCE) else {
            continue;
        };
        if distance == 0 {
            return Some(candidate);
        }
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
        assert_eq!(best_suggestion("NAME", ["Name", "name"]), Some("Name"));
        assert_eq!(best_suggestion("éab", ["Éab", "eab"]), None);
    }

    fn reference_distance(a: &str, b: &str) -> usize {
        let a = a
            .chars()
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let b = b
            .chars()
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let mut row = (0..=b.len()).collect::<Vec<_>>();
        for (i, a) in a.iter().enumerate() {
            let mut diagonal = row[0];
            row[0] = i + 1;
            for (j, b) in b.iter().enumerate() {
                let previous = row[j + 1];
                row[j + 1] = (previous + 1)
                    .min(row[j] + 1)
                    .min(diagonal + usize::from(a != b));
                diagonal = previous;
            }
        }
        row[b.len()]
    }

    #[test]
    fn reused_banded_distance_matches_full_unicode_distance() {
        let mut words = vec![String::new()];
        let mut level = vec![String::new()];
        for _ in 0..4 {
            level = level
                .iter()
                .flat_map(|word| ['a', 'B', 'é'].map(|ch| format!("{word}{ch}")))
                .collect();
            words.extend(level.iter().cloned());
        }
        words.extend(["country_flag", "COUNTRY_flag", "国é_flag"].map(str::to_owned));
        for a in &words {
            let mut workspace = DistanceWorkspace::new(a);
            for b in &words {
                let distance = reference_distance(a, b);
                for max in 0..=4 {
                    assert_eq!(
                        workspace.distance(b, max),
                        (distance <= max).then_some(distance),
                        "{a:?} {b:?} {max}"
                    );
                }
            }
        }
    }
}
