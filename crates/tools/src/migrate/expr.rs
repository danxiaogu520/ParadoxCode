//! Old-matcher → rules-v2 type-expression rendering for `rules-migrate`.
//!
//! The legacy `KeyMatcher`/`ValueMatcher` split collapses into the single
//! expression mini-syntax of `docs/rules-language.md` §2. Every renderer is a
//! pure function of the matcher plus the normalization tables
//! (`member_kind_aliases`, `scope_member_aliases`) so the conversion stays
//! deterministic and re-runnable.

use std::collections::BTreeMap;

use rules::{KeyMatcher, ValueMatcher};

/// Normalization tables read from the legacy profile.
pub struct Norm {
    /// `profile.lexicon.member_kind_aliases`: spelling → workspace symbol kind.
    pub aliases: BTreeMap<String, String>,
    /// `profile.scopes.scope_member_aliases`: link spelling → scope type.
    pub scope_aliases: BTreeMap<String, String>,
    /// Names of all legacy static enums (for `enum<…>` spelling checks).
    pub enum_names: Vec<String>,
    /// Names of every symbol type that has a definition site. A reference
    /// spelled `enum:` that names one of these is a symbol reference, not an
    /// enum member.
    pub type_names: std::collections::BTreeSet<String>,
}

impl Norm {
    /// Canonicalises a `ref`/`def` type spelling through `member_kind_aliases`.
    ///
    /// Dotted names (`event.country`) normalise their first segment only: the
    /// segment after the dot is a subtype name in the same namespace.
    #[must_use]
    pub fn type_name(&self, name: &str) -> String {
        if let Some(target) = self.aliases.get(name) {
            return target.clone();
        }
        if let Some((head, tail)) = name.split_once('.')
            && let Some(target) = self.aliases.get(head)
        {
            return format!("{target}.{tail}");
        }
        name.to_owned()
    }

    /// Canonicalises an `enum<…>` name. Enum names live in their own namespace
    /// and are only rewritten when the spelling is an alias whose target names
    /// an existing enum.
    #[must_use]
    pub fn enum_name(&self, name: &str) -> String {
        if self.enum_names.iter().any(|known| known == name) {
            return name.to_owned();
        }
        if let Some(target) = self.aliases.get(name)
            && self.enum_names.iter().any(|known| known == target)
        {
            return target.clone();
        }
        name.to_owned()
    }

    /// Canonicalises a scope spelling through `scope_member_aliases` (a link
    /// name such as `owner` becomes its target scope type `country`).
    #[must_use]
    pub fn scope_name(&self, name: &str) -> String {
        self.scope_aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    }

    /// Renders an `enum:` reference.
    ///
    /// Legacy rules use `enum:` for anything with a fixed member list, but a
    /// few names are symbol types with definition sites rather than enums
    /// (`government_attributes` is defined in `common/government_reforms`).
    /// Those become symbol references; anything else stays an enum, which the
    /// caller either declares or reports as a stub.
    #[must_use]
    pub fn enum_or_ref(&self, name: &str) -> String {
        let canonical = self.enum_name(name);
        if self.enum_names.iter().any(|known| known == &canonical) {
            return format!("enum<{canonical}>");
        }
        let as_type = self.type_name(name);
        if self.type_names.contains(&as_type) {
            return format!("ref<{as_type}>");
        }
        format!("enum<{canonical}>")
    }
}

/// Renders a value matcher as a type expression.
#[must_use]
pub fn value_expr(value: &ValueMatcher, norm: &Norm) -> String {
    match value {
        ValueMatcher::AnyScalar => "scalar".to_owned(),
        ValueMatcher::Exact(spelling) => literal(spelling),
        ValueMatcher::Bool => "bool".to_owned(),
        ValueMatcher::Int { min, max } => range(
            "int",
            min.map(|v| v.to_string()),
            max.map(|v| v.to_string()),
        ),
        ValueMatcher::Float { min, max } => {
            range("float", min.map(float_spelling), max.map(float_spelling))
        }
        ValueMatcher::Date => "date".to_owned(),
        ValueMatcher::Type(name) => format!("ref<{}>", norm.type_name(name)),
        ValueMatcher::Enum(name) => norm.enum_or_ref(name),
        ValueMatcher::Scope(scope) => format!(
            "scope<{}>",
            scope
                .as_deref()
                .map_or_else(|| "any".to_owned(), |value| norm.scope_name(value))
        ),
        ValueMatcher::Localisation => "loc".to_owned(),
        ValueMatcher::Filepath => "path".to_owned(),
        ValueMatcher::TexturePath => "path<gfx>".to_owned(),
        ValueMatcher::Dynamic(name) => format!("ref<{}>", norm.type_name(name)),
        ValueMatcher::DynamicSet(name) => format!("def<{}>", norm.type_name(name)),
        ValueMatcher::TypedPrefix { prefix, .. } => {
            // `trigger_value:NAME` becomes the template of §2.2; the old
            // operand filter (`numeric_or_bool`) has no expression form and is
            // reported by the caller.
            format!("'{}{{ref<scripted_trigger>}}'", escape_literal(prefix))
        }
        ValueMatcher::Opaque(_) => "opaque".to_owned(),
    }
}

/// Renders a key matcher as the `key` expression of a pattern.
///
/// Exact keys never render here: they become `fields` entries.
#[must_use]
pub fn key_expr(key: &KeyMatcher, norm: &Norm) -> String {
    match key {
        KeyMatcher::Exact(spelling) => literal(spelling),
        KeyMatcher::Type(name) => format!("ref<{}>", norm.type_name(name)),
        KeyMatcher::Enum(name) => norm.enum_or_ref(name),
        KeyMatcher::Dynamic(name) => format!("def<{}>", norm.type_name(name)),
        KeyMatcher::AnyScalar => "scalar".to_owned(),
        KeyMatcher::Int { min, max } => range(
            "int",
            min.map(|v| v.to_string()),
            max.map(|v| v.to_string()),
        ),
        KeyMatcher::Date => "date".to_owned(),
        KeyMatcher::Template {
            prefix,
            parameter,
            suffix,
        } => {
            // The legacy template parameter may strip an affix from the
            // resolved member name (`estate_burghers` → `burghers`); the
            // grammar spells that as the `strip_prefix` clause on the hole.
            let strip = parameter
                .strip_prefix
                .as_deref()
                .filter(|prefix| !prefix.is_empty())
                .map_or_else(String::new, |prefix| format!(" strip_prefix {prefix}"));
            let hole = match (parameter.type_domain(), parameter.enum_domain()) {
                (Some(name), _) => format!("ref<{}{strip}>", norm.type_name(name)),
                (None, Some(name)) => format!("enum<{}{strip}>", norm.enum_name(name)),
                (None, None) => "scalar".to_owned(),
            };
            format!(
                "'{}{{{}}}{}'",
                escape_literal(prefix),
                hole,
                escape_literal(suffix)
            )
        }
    }
}

/// The `parent_path` segment spelling used by the legacy importer for one key
/// matcher, used to correlate flat rows into a tree.
#[must_use]
pub fn key_segment(key: &KeyMatcher) -> String {
    match key {
        KeyMatcher::Exact(spelling) => spelling.to_lowercase(),
        KeyMatcher::Type(name) => format!("<{}>", name.to_lowercase()),
        KeyMatcher::Dynamic(name) => format!("<{}>", name.to_lowercase()),
        KeyMatcher::Enum(name) => format!("enum[{}]", name.to_lowercase()),
        KeyMatcher::AnyScalar => "any_scalar".to_owned(),
        KeyMatcher::Int { .. } => "int".to_owned(),
        // The legacy `parent_path` spells a date-keyed block as the pseudo
        // segment `date_field`; a `date` matcher must resolve to the same
        // position, or the declaring row and its rows land in two schemas.
        KeyMatcher::Date => "date_field".to_owned(),
        KeyMatcher::Template {
            prefix,
            parameter,
            suffix,
        } => {
            let domain = parameter
                .type_domain()
                .or(parameter.enum_domain())
                .unwrap_or("scalar");
            format!("<{prefix}_{domain}{suffix}>").to_lowercase()
        }
    }
}

/// Whether the key matcher is an exact, case-insensitive literal.
#[must_use]
pub fn is_exact(key: &KeyMatcher) -> bool {
    matches!(key, KeyMatcher::Exact(_))
}

/// Renders `'…'` literal text with the escapes of §2.1.
#[must_use]
pub fn escape_literal(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\'' | '\\' | '{' | '}' => {
                rendered.push('\\');
                rendered.push(character);
            }
            _ => rendered.push(character),
        }
    }
    rendered
}

/// Renders a constant literal expression.
#[must_use]
pub fn literal(text: &str) -> String {
    format!("'{}'", escape_literal(text))
}

/// Joins union branches (§2.1: `alt { "|" alt }`) in first-seen order,
/// dropping repeats.
#[must_use]
pub fn union(branches: Vec<String>) -> String {
    let mut seen = Vec::new();
    for branch in branches {
        if !seen.contains(&branch) {
            seen.push(branch);
        }
    }
    seen.join(" | ")
}

fn range(kind: &str, min: Option<String>, max: Option<String>) -> String {
    if min.is_none() && max.is_none() {
        return kind.to_owned();
    }
    format!(
        "{kind}[{}..{}]",
        min.unwrap_or_default(),
        max.unwrap_or_default()
    )
}

fn float_spelling(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm() -> Norm {
        Norm {
            aliases: BTreeMap::from([("country_tags".to_owned(), "country_tag".to_owned())]),
            scope_aliases: BTreeMap::from([("owner".to_owned(), "country".to_owned())]),
            enum_names: vec!["country_tags".to_owned()],
            type_names: std::collections::BTreeSet::from([
                "country_tag".to_owned(),
                "government_attributes".to_owned(),
            ]),
        }
    }

    /// An `enum:` spelling that names a symbol type with definition sites is a
    /// reference; a real static enum keeps its `enum<>` form.
    #[test]
    fn enum_or_ref_prefers_a_static_enum_over_a_type() {
        let norm = norm();
        assert_eq!(
            norm.enum_or_ref("government_attributes"),
            "ref<government_attributes>"
        );
        assert_eq!(norm.enum_or_ref("country_tags"), "enum<country_tags>");
    }

    #[test]
    fn literals_escape_grammar_characters() {
        assert_eq!(literal("it's {x}"), r#"'it\'s \{x\}'"#);
    }

    #[test]
    fn type_aliases_normalise_but_enums_keep_their_namespace() {
        assert_eq!(
            value_expr(&ValueMatcher::Type("country_tags".to_owned()), &norm()),
            "ref<country_tag>"
        );
        assert_eq!(
            value_expr(&ValueMatcher::Enum("country_tags".to_owned()), &norm()),
            "enum<country_tags>"
        );
    }

    #[test]
    fn scope_aliases_normalise_link_names() {
        assert_eq!(
            value_expr(&ValueMatcher::Scope(Some("owner".to_owned())), &norm()),
            "scope<country>"
        );
    }

    #[test]
    fn templates_render_holes() {
        let key = KeyMatcher::Template {
            prefix: "monthly_".to_owned(),
            parameter: rules::TemplateParameter {
                type_name: Some("government_mechanic_power".to_owned()),
                enum_name: None,
                strip_prefix: None,
            },
            suffix: String::new(),
        };
        assert_eq!(
            key_expr(&key, &norm()),
            "'monthly_{ref<government_mechanic_power>}'"
        );
    }

    #[test]
    fn key_segments_spell_like_the_legacy_parent_paths() {
        assert_eq!(
            key_segment(&KeyMatcher::Type("religious_school".to_owned())),
            "<religious_school>"
        );
        assert_eq!(
            key_segment(&KeyMatcher::Enum("country_tags".to_owned())),
            "enum[country_tags]"
        );
        assert_eq!(
            key_segment(&KeyMatcher::Exact("Religious_Schools".to_owned())),
            "religious_schools"
        );
    }
}
