//! Human-readable constraint text for diagnostics.
//!
//! Semantic rules are an internal index — source files, line numbers, matcher
//! tables. This module renders what a rule *requires* (value kinds, numeric
//! bounds, member lists) so diagnostic messages talk about the user's script
//! and never about the index. Every helper here is presentation-only: no
//! message may embed rule provenance or matcher vocabulary such as `int`,
//! `enum[...]`, or "value clause".

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
    fn did_you_mean_renders_only_with_suggestion() {
        assert_eq!(did_you_mean(Some("historic")), "; did you mean `historic`?");
        assert_eq!(did_you_mean(None), "");
    }
}
