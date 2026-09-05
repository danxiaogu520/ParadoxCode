//! Human-readable constraint text for diagnostics.
//!
//! Semantic rules are an internal index — source files, line numbers, matcher
//! tables. This module renders what a rule *requires* (value kinds, numeric
//! bounds, member lists) so diagnostic messages talk about the user's script
//! and never about the index. Every helper here is presentation-only: no
//! message may embed rule provenance or matcher vocabulary such as `int`,
//! `enum[...]`, or "value clause".

use pdx_engine::AnalysisSnapshot;
use pdx_rules::{KeyMatcher, ValueMatcher};

use crate::semantic::enum_members;

/// How many list members are shown before the "... and N more" tail.
const LIST_LIMIT: usize = 8;

/// Renders a deduplicated backticked list, truncating long enumerations.
///
/// `a, b, c` becomes `` `a`, `b`, `c` ``; a list past the limit keeps the
/// first `limit` entries and closes with `and N more` so huge first-party
/// enums stay readable.
pub(crate) fn backticked_list(items: &[&str], limit: usize) -> String {
    let mut unique: Vec<&str> = Vec::with_capacity(items.len());
    for item in items {
        if !unique.iter().any(|seen| seen.eq_ignore_ascii_case(item)) {
            unique.push(item);
        }
    }
    let shown = unique
        .iter()
        .take(limit)
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ");
    match unique.len().saturating_sub(limit) {
        0 => shown,
        extra => format!("{shown} and {extra} more"),
    }
}

/// "once", "twice", "3 times".
pub(crate) fn occurrence_word(count: u32) -> String {
    match count {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        count => format!("{count} times"),
    }
}

/// "a" or "an", agreeing with the word that follows.
pub(crate) fn article_for(word: &str) -> &'static str {
    match word
        .trim()
        .chars()
        .next()
        .map(|first| first.to_ascii_lowercase())
    {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// Renders the accepted values of a matcher as a singular noun phrase.
///
/// The result reads after "expected ": `expected one of \`a\`, \`b\``, or
/// `expected a whole number between 0 and 255`.
pub(crate) fn value_description(snapshot: &AnalysisSnapshot, matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar | ValueMatcher::DynamicSet(_) => "any value".to_owned(),
        ValueMatcher::Exact(value) => format!("`{value}`"),
        ValueMatcher::Bool => "`yes` or `no`".to_owned(),
        ValueMatcher::Int { min, max } => {
            let noun = "a whole number";
            match (min, max) {
                (Some(min), Some(max)) => format!("{noun} between {min} and {max}"),
                (Some(min), None) => format!("{noun} of at least {min}"),
                (None, Some(max)) => format!("{noun} of at most {max}"),
                (None, None) => noun.to_owned(),
            }
        }
        ValueMatcher::Float { min, max } => {
            let noun = "a number";
            match (min.as_deref(), max.as_deref()) {
                (Some(min), Some(max)) => format!("{noun} between {min} and {max}"),
                (Some(min), None) => format!("{noun} of at least {min}"),
                (None, Some(max)) => format!("{noun} of at most {max}"),
                (None, None) => noun.to_owned(),
            }
        }
        ValueMatcher::Date => "a date, such as 1444.11.11".to_owned(),
        ValueMatcher::Type(kind) => format!("{} `{kind}` name", article_for(kind)),
        ValueMatcher::Enum(name) => enum_members(snapshot, name).map_or_else(
            || format!("an accepted `{name}` value"),
            |members| {
                let members = members.iter().map(String::as_str).collect::<Vec<_>>();
                format!("one of {}", backticked_list(&members, LIST_LIMIT))
            },
        ),
        ValueMatcher::Scope(Some(name)) => format!("a `{name}` scope"),
        ValueMatcher::Scope(None) => "a scope name".to_owned(),
        ValueMatcher::Localisation => "a localisation key".to_owned(),
        ValueMatcher::Filepath => "a file path".to_owned(),
        ValueMatcher::Dynamic(kind) => format!("{} `{kind}` name", article_for(kind)),
        ValueMatcher::Opaque(value) => format!("`{value}`"),
    }
}

/// Renders the accepted values of a matcher as a plural noun phrase.
///
/// The result reads after "at least N ": `at least 3 whole numbers`, or
/// `at least 2 values from: \`a\`, \`b\``.
pub(crate) fn value_plural(snapshot: &AnalysisSnapshot, matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar | ValueMatcher::DynamicSet(_) => "values".to_owned(),
        ValueMatcher::Exact(value) => format!("`{value}` values"),
        ValueMatcher::Bool => "`yes`/`no` values".to_owned(),
        ValueMatcher::Int { min, max } => match (min, max) {
            (Some(min), Some(max)) => format!("whole numbers between {min} and {max}"),
            (Some(min), None) => format!("whole numbers of at least {min}"),
            (None, Some(max)) => format!("whole numbers of at most {max}"),
            (None, None) => "whole numbers".to_owned(),
        },
        ValueMatcher::Float { min, max } => match (min.as_deref(), max.as_deref()) {
            (Some(min), Some(max)) => format!("numbers between {min} and {max}"),
            (Some(min), None) => format!("numbers of at least {min}"),
            (None, Some(max)) => format!("numbers of at most {max}"),
            (None, None) => "numbers".to_owned(),
        },
        ValueMatcher::Date => "dates".to_owned(),
        ValueMatcher::Type(kind) => format!("`{kind}` names"),
        ValueMatcher::Enum(name) => enum_members(snapshot, name).map_or_else(
            || format!("`{name}` values"),
            |members| {
                let members = members.iter().map(String::as_str).collect::<Vec<_>>();
                format!("values from: {}", backticked_list(&members, LIST_LIMIT))
            },
        ),
        ValueMatcher::Scope(Some(name)) => format!("`{name}` scopes"),
        ValueMatcher::Scope(None) => "scope names".to_owned(),
        ValueMatcher::Localisation => "localisation keys".to_owned(),
        ValueMatcher::Filepath => "file paths".to_owned(),
        ValueMatcher::Dynamic(kind) => format!("`{kind}` names"),
        ValueMatcher::Opaque(value) => format!("`{value}` values"),
    }
}

/// Renders a key matcher as a noun phrase for cardinality messages.
pub(crate) fn key_description(matcher: &KeyMatcher) -> String {
    match matcher {
        KeyMatcher::Exact(value) => format!("`{value}`"),
        KeyMatcher::Type(kind) => format!("a `{kind}` name"),
        KeyMatcher::Enum(_) => "an accepted key".to_owned(),
        KeyMatcher::AnyScalar | KeyMatcher::Dynamic(_) | KeyMatcher::Date => "a key".to_owned(),
    }
}

/// Joins phrases as "`a`, `b`, or `c`".
fn join_phrases(phrases: &[String]) -> String {
    match phrases.split_last() {
        Some((last, head)) if !head.is_empty() => format!("{}, or {last}", head.join(", ")),
        Some((only, _)) => only.clone(),
        None => String::new(),
    }
}

/// Joins the value matchers of several rules into one expected phrase.
///
/// Duplicate phrases collapse (overloaded rule rows usually share a
/// matcher); the union keeps `or` semantics so mixed constraints stay
/// truthful, and truncates so a wide union cannot flood the message.
pub(crate) fn expected_from_rules<'a>(
    snapshot: &AnalysisSnapshot,
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
) -> Option<String> {
    let mut phrases: Vec<String> = Vec::new();
    for rule in rules {
        let phrase = value_description(snapshot, &rule.value);
        if !phrases
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&phrase))
        {
            phrases.push(phrase);
        }
    }
    match phrases.len() {
        0 => None,
        _ => {
            let visible = phrases.iter().take(3).cloned().collect::<Vec<_>>();
            let shown = join_phrases(&visible);
            match phrases.len().saturating_sub(3) {
                0 => Some(shown),
                extra => Some(format!("{shown}, or {extra} more constraints")),
            }
        }
    }
}

/// Formats a did-you-mean suffix, or an empty string when there is no
/// confident suggestion.
pub(crate) fn did_you_mean(suggestion: Option<&str>) -> String {
    match suggestion {
        Some(candidate) => format!("; did you mean `{candidate}`?"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backticked_list_truncates_and_dedups() {
        assert_eq!(backticked_list(&["a", "b"], 8), "`a`, `b`");
        assert_eq!(backticked_list(&["a", "A", "b"], 8), "`a`, `b`");
        assert_eq!(
            backticked_list(&["a", "b", "c", "d"], 2),
            "`a`, `b` and 2 more"
        );
    }

    #[test]
    fn occurrence_word_covers_small_counts() {
        assert_eq!(occurrence_word(1), "once");
        assert_eq!(occurrence_word(2), "twice");
        assert_eq!(occurrence_word(5), "5 times");
    }

    #[test]
    fn key_description_names_exact_keys_verbatim() {
        assert_eq!(
            key_description(&KeyMatcher::Exact("trigger".into())),
            "`trigger`"
        );
    }

    #[test]
    fn did_you_mean_renders_only_with_suggestion() {
        assert_eq!(did_you_mean(Some("historic")), "; did you mean `historic`?");
        assert_eq!(did_you_mean(None), "");
    }
}
