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
/// Named member domain resolving the parameter segment of a template key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateParameter {
    /// Workspace-indexed type whose members instantiate the parameter.
    #[serde(rename = "type")]
    pub type_name: Option<String>,
    /// Named static enum whose members instantiate the parameter.
    #[serde(rename = "enum")]
    pub enum_name: Option<String>,
    /// Prefix removed from a member before it is spliced into the key.
    ///
    /// Some domains name their members with a redundant prefix the key family
    /// drops: estates are `estate_nobles` while the modifier families spell
    /// `nobles_loyalty_modifier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<String>,
}

impl TemplateParameter {
    /// The workspace type domain, when the parameter resolves through one.
    #[must_use]
    pub fn type_domain(&self) -> Option<&str> {
        self.type_name.as_deref()
    }

    /// The static enum domain, when the parameter resolves through one.
    #[must_use]
    pub fn enum_domain(&self) -> Option<&str> {
        self.enum_name.as_deref()
    }

    /// The key spelling of one domain member: the declared prefix removed when
    /// the member carries it, the member verbatim otherwise.
    #[must_use]
    pub fn splice_member<'member>(&self, member: &'member str) -> &'member str {
        match self.strip_prefix.as_deref() {
            Some(prefix)
                if !prefix.is_empty()
                    && member
                        .get(..prefix.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(prefix)) =>
            {
                &member[prefix.len()..]
            }
            _ => member,
        }
    }

    /// Tests the parameter spelling from a key against the declared domain.
    ///
    /// With `strip_prefix` set to `P`, the engine derives the key spelling by
    /// removing `P` from the member, so `P + spelling` and a bare `spelling`
    /// (members that never carried the prefix) are both accepted — unless the
    /// spelling itself starts with `P`, which would double-count the prefix.
    fn matches(
        &self,
        spelling: &str,
        type_members: impl Fn(&str, &str) -> bool,
        enum_members: impl Fn(&str, &str) -> bool,
    ) -> bool {
        let is_member = |name: &str| match (&self.type_name, &self.enum_name) {
            (Some(type_name), _) => type_members(type_name, name),
            (None, Some(enum_name)) => enum_members(enum_name, name),
            (None, None) => false,
        };
        if let Some(prefix) = self.strip_prefix.as_deref()
            && !prefix.is_empty()
            && strip_prefix_ascii_ci(spelling, prefix).is_some()
        {
            // The spelling already carries the prefix the engine removes once;
            // every acceptance path would double-count it.
            return false;
        }
        if is_member(spelling) {
            return true;
        }
        let Some(prefix) = self.strip_prefix.as_deref() else {
            return false;
        };
        if prefix.is_empty() || spelling.is_empty() {
            return false;
        }
        let mut prefixed = String::with_capacity(prefix.len() + spelling.len());
        prefixed.push_str(prefix);
        prefixed.push_str(spelling);
        is_member(&prefixed)
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
    /// Matches `<prefix><member><suffix>` where `member` resolves through a
    /// named workspace type or static enum, for example the parameterized
    /// modifier families `monthly_<government_mechanic_power>` and
    /// `<estate>_loyalty_modifier`.
    Template {
        /// Literal key prefix before the parameter segment.
        prefix: String,
        /// Named member domain for the parameter segment.
        parameter: TemplateParameter,
        /// Literal key suffix after the parameter segment.
        suffix: String,
    },
    /// Matches any non-empty scalar key.
    AnyScalar,
    /// Matches an integer key, optionally constrained by an inclusive range
    /// (for example the rank numbers in `government_ranks`).
    Int { min: Option<i64>, max: Option<i64> },
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
            Self::Template {
                prefix,
                parameter,
                suffix,
            } => {
                let Some(after_prefix) = strip_prefix_ascii_ci(key, prefix) else {
                    return false;
                };
                let Some(spelling) = strip_suffix_ascii_ci(after_prefix, suffix) else {
                    return false;
                };
                !spelling.is_empty() && parameter.matches(spelling, type_members, enum_members)
            }
            Self::AnyScalar => !key.is_empty(),
            Self::Int { min, max } => key.parse::<i64>().is_ok_and(|value| {
                min.is_none_or(|bound| value >= bound) && max.is_none_or(|bound| value <= bound)
            }),
            Self::Date => is_eu4_date(key),
            Self::Dynamic(_) => !key.is_empty(),
        }
    }
}

/// Removes a case-insensitive ASCII prefix, leaving the remainder.
fn strip_prefix_ascii_ci<'key>(key: &'key str, prefix: &str) -> Option<&'key str> {
    if prefix.is_empty() {
        return Some(key);
    }
    key.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        .then(|| &key[prefix.len()..])
}

/// Removes a case-insensitive ASCII suffix, leaving the remainder.
fn strip_suffix_ascii_ci<'key>(key: &'key str, suffix: &str) -> Option<&'key str> {
    if suffix.is_empty() {
        return Some(key);
    }
    let boundary = key.len().checked_sub(suffix.len())?;
    key.get(boundary..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
        .then(|| &key[..boundary])
}

/// Operand shape accepted on the alias rows a typed-prefix reference resolves to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedPrefixOperand {
    /// The referenced alias rows must accept an int, float, or bool operand, such as
    /// `trigger_value:<numeric trigger>`.
    NumericOrBool,
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
    /// Accepts `prefix<name>` where `name` resolves to a top-level alias row of `context`
    /// whose operand matches. The generic matcher below only checks the literal prefix and
    /// a non-empty name; the alias resolution runs in the analysis layer, which owns the
    /// compiled rule set.
    TypedPrefix {
        /// Literal prefix including its trailing colon.
        prefix: String,
        /// Rule context whose top-level alias rows resolve the name.
        context: String,
        /// Operand shapes accepted on the resolved alias rows.
        operand: TypedPrefixOperand,
    },
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
            // The typed-prefix alias resolution needs the compiled rule set; this generic
            // path only rejects a missing prefix or an empty referenced name.
            Self::TypedPrefix { prefix, .. } => {
                value
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                    && !value[prefix.len()..].is_empty()
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn int_matches(min: Option<i64>, max: Option<i64>, key: &str) -> bool {
        KeyMatcher::Int { min, max }.matches(key, |_, _| false, |_, _| false)
    }

    fn estate_template(suffix: &str) -> KeyMatcher {
        KeyMatcher::Template {
            prefix: String::new(),
            parameter: TemplateParameter {
                type_name: Some("estate".to_owned()),
                enum_name: None,
                strip_prefix: Some("estate_".to_owned()),
            },
            suffix: suffix.to_owned(),
        }
    }

    fn template_matches(matcher: &KeyMatcher, key: &str) -> bool {
        matcher.matches(
            key,
            |type_name, member| {
                type_name == "estate" && member.eq_ignore_ascii_case("estate_nobles")
            },
            |_, _| false,
        )
    }

    #[test]
    fn template_matches_workspace_members_with_stripped_prefix() {
        let matcher = estate_template("_loyalty_modifier");
        assert!(template_matches(&matcher, "nobles_loyalty_modifier"));
        assert!(template_matches(&matcher, "NOBLES_LOYALTY_MODIFIER"));
        assert!(!template_matches(&matcher, "burghers_loyalty_modifier"));
        assert!(!template_matches(&matcher, "nobles_loyalty_modifier_extra"));
        assert!(!template_matches(&matcher, "_loyalty_modifier"));
        assert!(!template_matches(&matcher, "nobles_"));
    }

    #[test]
    fn template_rejects_spellings_that_double_count_the_strip_prefix() {
        let matcher = estate_template("_loyalty_modifier");
        // `estate_nobles_loyalty_modifier` would reconstruct `estate_estate_nobles`;
        // the engine derives the key from the prefix-stripped estate name.
        assert!(!template_matches(
            &matcher,
            "estate_nobles_loyalty_modifier"
        ));
    }

    #[test]
    fn template_accepts_members_without_the_strip_prefix() {
        // A mod estate defined as `nobles` (no `estate_` prefix) spells the same key.
        let matcher = estate_template("_loyalty_modifier");
        assert!(template_matches(&matcher, "nobles_loyalty_modifier",));
    }

    #[test]
    fn template_matches_prefixed_power_family() {
        let matcher = KeyMatcher::Template {
            prefix: "monthly_".to_owned(),
            parameter: TemplateParameter {
                type_name: Some("government_mechanic_power".to_owned()),
                enum_name: None,
                strip_prefix: None,
            },
            suffix: String::new(),
        };
        let matches = |key: &str| {
            matcher.matches(
                key,
                |type_name, member| {
                    type_name == "government_mechanic_power"
                        && member.eq_ignore_ascii_case("russian_modernization")
                },
                |_, _| false,
            )
        };
        assert!(matches("monthly_russian_modernization"));
        assert!(matches("Monthly_Russian_Modernization"));
        assert!(!matches("monthly_reform_progress"));
        assert!(!matches("monthly_"));
    }

    #[test]
    fn template_matches_static_enum_domain() {
        let matcher = KeyMatcher::Template {
            prefix: String::new(),
            parameter: TemplateParameter {
                type_name: None,
                enum_name: Some("estate_all".to_owned()),
                strip_prefix: None,
            },
            suffix: "_bonus".to_owned(),
        };
        let matches = |key: &str| {
            matcher.matches(
                key,
                |_, _| false,
                |enum_name, member| enum_name == "estate_all" && member == "all",
            )
        };
        assert!(matches("all_bonus"));
        assert!(!matches("any_bonus"));
    }

    #[test]
    fn template_parameter_splices_member_spellings() {
        let parameter = TemplateParameter {
            type_name: Some("estate".to_owned()),
            enum_name: None,
            strip_prefix: Some("estate_".to_owned()),
        };
        assert_eq!(parameter.splice_member("estate_nobles"), "nobles");
        assert_eq!(parameter.splice_member("ESTATE_NOBLES"), "NOBLES");
        assert_eq!(parameter.splice_member("my_guild"), "my_guild");
        let plain = TemplateParameter {
            type_name: Some("government_mechanic_power".to_owned()),
            enum_name: None,
            strip_prefix: None,
        };
        assert_eq!(plain.splice_member("blood"), "blood");
    }

    #[test]
    fn int_key_enforces_inclusive_bounds() {
        assert!(int_matches(Some(1), Some(10), "1"));
        assert!(int_matches(Some(1), Some(10), "10"));
        assert!(!int_matches(Some(1), Some(10), "0"));
        assert!(!int_matches(Some(1), Some(10), "11"));
        assert!(!int_matches(Some(1), Some(10), "-1"));
    }

    #[test]
    fn int_key_accepts_open_ranges() {
        assert!(int_matches(None, None, "-42"));
        assert!(int_matches(Some(3), None, "9000"));
        assert!(!int_matches(Some(3), None, "2"));
        assert!(int_matches(None, Some(0), "-1"));
        assert!(!int_matches(None, Some(0), "1"));
    }

    #[test]
    fn int_key_rejects_non_integers() {
        assert!(!int_matches(None, None, "abc"));
        assert!(!int_matches(None, None, "2.5"));
        assert!(!int_matches(None, None, "1444.11.11"));
        assert!(!int_matches(None, None, ""));
    }
}
