use pdx_text::LogicalPath;
use serde::{Deserialize, Serialize};
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
/// A key matcher compiled from a first-party field declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMatcher {
    /// Matches one concrete script key.
    Exact(String),
    /// Matches a key supplied by the workspace index for a named type.
    Type(String),
    /// Matches a member of a named static enum.
    Enum(String),
    /// Matches any non-empty scalar key.
    AnyScalar,
    /// Matches a campaign date key such as `1444.11.11`.
    Date,
    /// Matches a key that declares a dynamic value set.
    Dynamic(String),
}

impl KeyMatcher {
    /// Tests a key against static and workspace-provided members.
    #[must_use]
    pub fn matches(
        &self,
        key: &str,
        type_members: impl Fn(&str, &str) -> bool,
        enum_members: impl Fn(&str, &str) -> bool,
    ) -> bool {
        match self {
            Self::Exact(expected) => expected.eq_ignore_ascii_case(key),
            Self::Type(type_name) => type_members(type_name, key),
            Self::Enum(enum_name) => enum_members(enum_name, key),
            Self::AnyScalar => !key.is_empty(),
            Self::Date => is_eu4_date(key),
            Self::Dynamic(_) => !key.is_empty(),
        }
    }
}

/// A value matcher compiled from a first-party field declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueMatcher {
    /// Accepts any scalar value.
    AnyScalar,
    /// Accepts one exact scalar value.
    Exact(String),
    /// Accepts `yes` or `no`.
    Bool,
    /// Accepts an integer, optionally constrained by an inclusive range.
    Int { min: Option<i64>, max: Option<i64> },
    /// Accepts a floating point value, optionally constrained by an inclusive range.
    Float {
        min: Option<String>,
        max: Option<String>,
    },
    /// Accepts a campaign date such as `1444.11.11`, `1444.11`, or `1444`.
    Date,
    /// Accepts a member supplied by the workspace index.
    Type(String),
    /// Accepts a member of a named static enum.
    Enum(String),
    /// Accepts a known scope name.
    Scope(Option<String>),
    /// Accepts a localisation key.
    Localisation,
    /// Accepts a path-like scalar.
    Filepath,
    /// Accepts a workspace- or scope-derived value set.
    Dynamic(String),
    /// Accepts any non-empty value while defining a dynamic value set.
    DynamicSet(String),
    /// Retains a semantic matcher that has not been implemented yet.
    Opaque(String),
}

impl ValueMatcher {
    /// Tests a scalar value against the compiled matcher.
    #[must_use]
    pub fn matches(
        &self,
        value: &str,
        type_members: impl Fn(&str, &str) -> bool,
        enum_members: impl Fn(&str, &str) -> bool,
        scopes: impl Fn(Option<&str>, &str) -> bool,
    ) -> bool {
        match self {
            Self::AnyScalar | Self::Opaque(_) => true,
            Self::Exact(expected) => expected.eq_ignore_ascii_case(value),
            Self::Bool => matches!(value.to_ascii_lowercase().as_str(), "yes" | "no"),
            Self::Int { min, max } => {
                let Ok(value) = value.parse::<i64>() else {
                    return false;
                };
                min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
            }
            Self::Float { min, max } => {
                let Ok(value) = value.parse::<f64>() else {
                    return false;
                };
                let lower = min.as_deref().and_then(|min| min.parse::<f64>().ok());
                let upper = max.as_deref().and_then(|max| max.parse::<f64>().ok());
                lower.is_none_or(|min| value >= min) && upper.is_none_or(|max| value <= max)
            }
            Self::Date => is_eu4_date(value),
            Self::Type(type_name) => type_members(type_name, value),
            Self::Enum(enum_name) => enum_members(enum_name, value),
            Self::Scope(scope) => scopes(scope.as_deref(), value),
            // The game falls back to rendering the raw spelling, so an empty string is valid.
            Self::Localisation => true,
            Self::Filepath | Self::Dynamic(_) | Self::DynamicSet(_) => !value.is_empty(),
        }
    }
}

/// Tests whether a scalar is a campaign date such as `1444.11.11`, `1444.11`, or `1444`.
fn is_eu4_date(value: &str) -> bool {
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
