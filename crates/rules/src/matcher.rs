use serde::{Deserialize, Serialize};
use text::LogicalPath;
/// A path matcher from the rules file-category catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMatcher {
    /// Optional path prefix, without a leading slash.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Optional exact logical path, without a leading slash.
    #[serde(default)]
    pub path_exact: Option<String>,
    /// Accepted file extensions without the leading dot.
    pub extensions: Vec<String>,
    /// Optional suffix match on the logical path.
    #[serde(default)]
    pub path_suffix: Option<String>,
    /// Directory-bounded prefixes that must not match this category.
    #[serde(default)]
    pub path_exclude_prefixes: Vec<String>,
    /// Whether path and extension matching preserves case.
    pub case_sensitive: bool,
}

impl FileMatcher {
    /// Returns a stable specificity key used to select the most precise category.
    #[must_use]
    pub(crate) fn specificity(&self) -> (u8, usize) {
        if let Some(path) = &self.path_exact {
            return (2, path.len());
        }
        (
            u8::from(self.path_prefix.is_some() || self.path_suffix.is_some()),
            self.path_prefix.as_ref().map_or(0, String::len)
                + self.path_suffix.as_ref().map_or(0, String::len),
        )
    }

    /// Matches a validated logical path.
    #[must_use]
    pub fn matches(&self, path: &LogicalPath) -> bool {
        let candidate = path.as_str();
        if let Some(exact) = &self.path_exact {
            let matches_exact = if self.case_sensitive {
                candidate == exact
            } else {
                candidate.eq_ignore_ascii_case(exact)
            };
            if !matches_exact {
                return false;
            }
        }
        if self
            .path_exclude_prefixes
            .iter()
            .any(|prefix| directory_prefix_matches(candidate, prefix, self.case_sensitive))
        {
            return false;
        }
        if let Some(prefix) = &self.path_prefix
            && !directory_prefix_matches(candidate, prefix, self.case_sensitive)
        {
            return false;
        }
        if let Some(suffix) = &self.path_suffix {
            let matches_suffix = if self.case_sensitive {
                candidate.ends_with(suffix)
            } else {
                candidate.len() >= suffix.len()
                    && candidate
                        .get(candidate.len() - suffix.len()..)
                        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
            };
            if !matches_suffix {
                return false;
            }
        }
        if self.extensions.is_empty() {
            return true;
        }
        let Some(extension) = candidate.rsplit_once('.').map(|(_, extension)| extension) else {
            return false;
        };
        self.extensions.iter().any(|item| {
            if self.case_sensitive {
                item == extension
            } else {
                item.eq_ignore_ascii_case(extension)
            }
        })
    }
}

fn directory_prefix_matches(candidate: &str, prefix: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        candidate == prefix
            || candidate
                .strip_prefix(prefix)
                .is_some_and(|remainder| remainder.starts_with('/'))
    } else {
        candidate.len() == prefix.len() && candidate.eq_ignore_ascii_case(prefix)
            || candidate.len() > prefix.len()
                && candidate
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                && candidate.as_bytes().get(prefix.len()) == Some(&b'/')
    }
}

/// Tests whether a scalar is a campaign date such as `1444.11.11`, `1444.11`, or `1444`.
pub(crate) fn is_eu4_date(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(year) = parts.next() else {
        return false;
    };
    if year.is_empty() || year.len() > 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut trailing = 0;
    for part in parts {
        trailing += 1;
        if part.is_empty() || part.len() > 2 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    trailing <= 2
}
