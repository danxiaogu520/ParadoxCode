//! Rule-aware, game-independent semantic lowering boundary.

use std::sync::Arc;

use pdx_rules::{GameProfile, ProfileDefinitionRule, RuleSet};
use pdx_syntax::{CstKind, CstNode, ParsedFile};
use pdx_text::{LogicalPath, TextRange};

/// A conservative semantic scope value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scope {
    /// No scope is known yet; later analysis must avoid cascading errors.
    Unknown,
    /// The root scope of a file.
    Root,
}

/// One scalar value attached directly to a property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirScalar {
    /// Unquoted, trimmed spelling.
    pub value: String,
    /// Exact source range including quotes when present.
    pub range: TextRange,
}

/// A property fact retained independently of game-specific interpretation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirProperty {
    /// Property key spelling.
    pub key: String,
    /// Exact key range.
    pub key_range: TextRange,
    /// Full property range.
    pub range: TextRange,
    /// Property key path from the document root.
    pub path: Vec<String>,
    /// Whether this property is a direct document child.
    pub top_level: bool,
    /// Exact value-wrapper range, when parsing recovered a value.
    pub value_range: Option<TextRange>,
    /// Direct scalar value, when the value is not only a block.
    pub scalar: Option<HirScalar>,
}

/// One localisation definition produced by the localisation frontend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirLocalisationEntry {
    /// Localisation key spelling.
    pub name: String,
    /// Full entry range.
    pub range: TextRange,
    /// Exact key range.
    pub name_range: TextRange,
}

/// One profile-interpreted symbol definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirDefinition {
    /// Stable workspace symbol kind.
    pub kind: String,
    /// Declared symbol spelling.
    pub name: String,
    /// Full declaration range.
    pub range: TextRange,
    /// Exact range that supplies the symbol name.
    pub selection_range: TextRange,
}

/// One profile- or category-interpreted symbol reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirReference {
    /// Stable target symbol kind.
    pub kind: String,
    /// Referenced symbol spelling.
    pub name: String,
    /// Exact source range of the reference.
    pub range: TextRange,
    /// Interpretation layer that emitted this reference.
    pub origin: HirReferenceOrigin,
}

/// The interpretation layer that emitted a HIR reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HirReferenceOrigin {
    /// A precise property matcher from the selected game profile.
    Profile,
    /// A conservative bare value associated with the file category.
    Category,
}

/// A lowered file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFile {
    syntax: Arc<ParsedFile>,
    scope: Scope,
    properties: Vec<HirProperty>,
    localisation_entries: Vec<HirLocalisationEntry>,
    bare_values: Vec<HirScalar>,
    definitions: Vec<HirDefinition>,
    references: Vec<HirReference>,
}

impl HirFile {
    /// Returns the source syntax handle.
    #[must_use]
    pub fn syntax(&self) -> &ParsedFile {
        &self.syntax
    }

    /// Returns the conservative file scope.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Returns lowered properties in source order.
    #[must_use]
    pub fn properties(&self) -> &[HirProperty] {
        &self.properties
    }

    /// Returns localisation definitions in source order.
    #[must_use]
    pub fn localisation_entries(&self) -> &[HirLocalisationEntry] {
        &self.localisation_entries
    }

    /// Returns unquoted value tokens that are not property keys.
    #[must_use]
    pub fn bare_values(&self) -> &[HirScalar] {
        &self.bare_values
    }

    /// Returns profile-interpreted definitions in deterministic source order.
    #[must_use]
    pub fn definitions(&self) -> &[HirDefinition] {
        &self.definitions
    }

    /// Returns profile- and category-interpreted references in deterministic source order.
    #[must_use]
    pub fn references(&self) -> &[HirReference] {
        &self.references
    }
}

/// Lowers a parsed PDX file into game-independent structural facts.
#[must_use]
pub fn lower(syntax: ParsedFile, rules: &RuleSet) -> HirFile {
    lower_shared(Arc::new(syntax), rules)
}

/// Lowers a shared parsed file without copying its CST.
#[must_use]
pub fn lower_shared(syntax: Arc<ParsedFile>, rules: &RuleSet) -> HirFile {
    lower_shared_impl(syntax, None, rules, None)
}

/// Lowers a parsed file with an explicitly selected game profile and logical path.
#[must_use]
pub fn lower_with_profile(
    syntax: ParsedFile,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
) -> HirFile {
    lower_shared_with_profile(Arc::new(syntax), logical_path, rules, profile)
}

/// Lowers a shared parsed file with profile-aware semantic interpretation.
#[must_use]
pub fn lower_shared_with_profile(
    syntax: Arc<ParsedFile>,
    logical_path: &LogicalPath,
    rules: &RuleSet,
    profile: &GameProfile,
) -> HirFile {
    lower_shared_impl(syntax, Some(logical_path), rules, Some(profile))
}

fn lower_shared_impl(
    syntax: Arc<ParsedFile>,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> HirFile {
    let (properties, localisation_entries, bare_values) = {
        let mut collector = FactCollector::new(&syntax);
        collector.collect(syntax.root(), true, false, &[]);
        (collector.properties, collector.localisation_entries, collector.bare_values)
    };
    let (definitions, references) = lower_semantics(
        &properties,
        &localisation_entries,
        &bare_values,
        logical_path,
        rules,
        profile,
    );
    HirFile {
        syntax,
        scope: Scope::Unknown,
        properties,
        localisation_entries,
        bare_values,
        definitions,
        references,
    }
}

fn lower_semantics(
    properties: &[HirProperty],
    localisation_entries: &[HirLocalisationEntry],
    bare_values: &[HirScalar],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> (Vec<HirDefinition>, Vec<HirReference>) {
    let mut definitions = localisation_entries
        .iter()
        .map(|entry| HirDefinition {
            kind: "localisation".to_owned(),
            name: entry.name.clone(),
            range: entry.range,
            selection_range: entry.name_range,
        })
        .collect::<Vec<_>>();
    let mut references = Vec::new();
    let path = logical_path.map_or("", LogicalPath::as_str);

    if let Some(profile) = profile {
        for property in properties {
            if property.top_level {
                if let Some(rule) = profile
                    .definition(path, &property.key)
                    .filter(|rule| !rule.requires_value || property.value_range.is_some())
                {
                    definitions.push(definition_from_rule(properties, property, rule));
                }
                for rule in
                    profile.conditional_definitions.iter().filter(|rule| rule.path.matches(path))
                {
                    if nested_property(properties, property, &rule.required_field)
                        .and_then(|nested| nested.scalar.as_ref())
                        .is_some_and(|scalar| {
                            scalar.value.eq_ignore_ascii_case(&rule.required_value)
                        })
                        && nested_property(properties, property, &rule.absent_field).is_none()
                    {
                        definitions.push(HirDefinition {
                            kind: rule.kind.clone(),
                            name: property.key.clone(),
                            range: property.range,
                            selection_range: property.key_range,
                        });
                    }
                }
                for rule in profile
                    .container_definitions
                    .iter()
                    .filter(|rule| rule.path.matches(path) && rule.key.matches(&property.key))
                {
                    for child in properties.iter().filter(|candidate| {
                        candidate.path.len() == property.path.len().saturating_add(1)
                            && candidate.path.starts_with(&property.path)
                            && range_within(candidate.range, property.range)
                    }) {
                        definitions.push(HirDefinition {
                            kind: rule.kind.clone(),
                            name: child.key.clone(),
                            range: child.range,
                            selection_range: child.key_range,
                        });
                    }
                }
            }
            if let Some(reference) = reference_from_property(profile, property) {
                references.push(reference);
            }
        }

        for property in properties {
            let parent_key = property.path.iter().rev().nth(1).map(String::as_str);
            if let Some(kind) = profile.value_definition_kind(&property.key, parent_key)
                && let Some(scalar) = property.scalar.as_ref()
                && !scalar.value.is_empty()
            {
                definitions.push(HirDefinition {
                    kind: kind.to_owned(),
                    name: scalar.value.clone(),
                    range: scalar.range,
                    selection_range: scalar.range,
                });
            }
        }
    }

    if let Some(category) = logical_path.and_then(|path| rules.classify(path)) {
        references.extend(bare_values.iter().map(|value| HirReference {
            kind: category.id.clone(),
            name: value.value.clone(),
            range: value.range,
            origin: HirReferenceOrigin::Category,
        }));
    }
    (definitions, references)
}

fn definition_from_rule(
    properties: &[HirProperty],
    property: &HirProperty,
    rule: &ProfileDefinitionRule,
) -> HirDefinition {
    let named = rule
        .name_field
        .as_deref()
        .and_then(|field| nested_property(properties, property, field))
        .and_then(|nested| nested.scalar.as_ref());
    HirDefinition {
        kind: rule.kind.clone(),
        name: named.map_or_else(|| property.key.clone(), |scalar| scalar.value.clone()),
        range: property.range,
        selection_range: named.map_or(property.key_range, |scalar| scalar.range),
    }
}

fn reference_from_property(profile: &GameProfile, property: &HirProperty) -> Option<HirReference> {
    let kind = profile.reference_kind(&property.key)?;
    let scalar = property.scalar.as_ref()?;
    if scalar.value.is_empty()
        || scalar.value.eq_ignore_ascii_case("yes")
        || scalar.value.eq_ignore_ascii_case("no")
        || scalar.value.parse::<f64>().is_ok()
    {
        return None;
    }
    Some(HirReference {
        kind: kind.to_owned(),
        name: scalar.value.clone(),
        range: scalar.range,
        origin: HirReferenceOrigin::Profile,
    })
}

fn nested_property<'hir>(
    properties: &'hir [HirProperty],
    parent: &HirProperty,
    wanted: &str,
) -> Option<&'hir HirProperty> {
    properties
        .iter()
        .filter(|property| property.path.len() > parent.path.len())
        .filter(|property| property.path.starts_with(&parent.path))
        .filter(|property| range_within(property.range, parent.range))
        .find(|property| property.key.eq_ignore_ascii_case(wanted))
}

fn range_within(inner: TextRange, outer: TextRange) -> bool {
    inner.start() >= outer.start() && inner.end() <= outer.end()
}

struct FactCollector<'syntax> {
    syntax: &'syntax ParsedFile,
    properties: Vec<HirProperty>,
    localisation_entries: Vec<HirLocalisationEntry>,
    bare_values: Vec<HirScalar>,
}

impl<'syntax> FactCollector<'syntax> {
    fn new(syntax: &'syntax ParsedFile) -> Self {
        Self {
            syntax,
            properties: Vec::new(),
            localisation_entries: Vec::new(),
            bare_values: Vec::new(),
        }
    }

    fn collect(
        &mut self,
        node: &CstNode,
        top_level: bool,
        inside_key: bool,
        parent_path: &[String],
    ) {
        if node.kind() == CstKind::LocalisationEntry
            && let Some(key) =
                node.children().iter().find(|child| child.kind() == CstKind::LocalisationKey)
            && let Some(name) = self.syntax.text(key.range())
        {
            self.localisation_entries.push(HirLocalisationEntry {
                name: name.trim().to_owned(),
                range: node.range(),
                name_range: key.range(),
            });
        }
        if node.kind() == CstKind::BareValue
            && !inside_key
            && let Some(value) =
                self.syntax.text(node.range()).map(str::trim).filter(|value| !value.is_empty())
        {
            self.bare_values.push(HirScalar { value: value.to_owned(), range: node.range() });
        }
        if node.kind() == CstKind::Property
            && let Some(key_node) =
                node.children().iter().find(|child| child.kind() == CstKind::Key)
            && let Some(key) =
                self.syntax.text(key_node.range()).map(str::trim).filter(|key| !key.is_empty())
        {
            let mut path = parent_path.to_vec();
            path.push(key.to_owned());
            self.properties.push(HirProperty {
                key: key.to_owned(),
                key_range: key_node.range(),
                range: node.range(),
                path: path.clone(),
                top_level,
                value_range: node
                    .children()
                    .iter()
                    .find(|child| child.kind() == CstKind::Value)
                    .map(CstNode::range),
                scalar: direct_scalar(self.syntax, node),
            });
            for child in node.children() {
                self.collect(child, false, inside_key || node.kind() == CstKind::Key, &path);
            }
            return;
        }
        for child in node.children() {
            self.collect(
                child,
                top_level && node.kind() == CstKind::Document,
                inside_key || node.kind() == CstKind::Key,
                parent_path,
            );
        }
    }
}

fn direct_scalar(syntax: &ParsedFile, node: &CstNode) -> Option<HirScalar> {
    let value = node.children().iter().find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .iter()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let raw = syntax.text(scalar.range())?.trim();
    let value =
        raw.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(raw).to_owned();
    Some(HirScalar { value, range: scalar.range() })
}

#[cfg(test)]
mod tests {
    use super::{lower, lower_with_profile};
    use pdx_game_eu4::{bootstrap_rules, profile};
    use pdx_rules::{GameProfile, RuleSet};
    use pdx_syntax::{Eu4FileFormat, parse_eu4};
    use pdx_text::LogicalPath;

    #[test]
    fn lowering_retains_property_paths_scalars_and_top_level_identity() {
        let parsed = parse_eu4(
            Eu4FileFormat::PdxScript,
            "root = { child = \"value\" nested = { leaf = yes } }\n",
        );
        let hir = lower(parsed, &RuleSet::empty());

        assert_eq!(hir.properties().len(), 4);
        assert!(hir.properties()[0].top_level);
        assert_eq!(hir.properties()[0].path, ["root"]);
        assert_eq!(hir.properties()[1].path, ["root", "child"]);
        assert!(hir.properties()[1].value_range.is_some());
        assert_eq!(hir.properties()[1].scalar.as_ref().expect("child scalar").value, "value");
        assert_eq!(hir.properties()[3].path, ["root", "nested", "leaf"]);
        assert_eq!(
            hir.bare_values().iter().map(|value| value.value.as_str()).collect::<Vec<_>>(),
            ["yes"]
        );
    }

    #[test]
    fn lowering_retains_localisation_definition_ranges() {
        let parsed =
            parse_eu4(Eu4FileFormat::Localisation, "l_english:\n example_key:0 \"Example\"\n");
        let hir = lower(parsed, &RuleSet::empty());

        assert_eq!(hir.localisation_entries().len(), 1);
        assert_eq!(hir.localisation_entries()[0].name, "example_key");
        let entry = &hir.localisation_entries()[0];
        assert!(entry.range.start() <= entry.name_range.start());
        assert!(entry.name_range.end() <= entry.range.end());
    }

    #[test]
    fn profile_aware_lowering_produces_shared_typed_definitions_and_references() {
        let rules = bootstrap_rules();
        let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
        let source =
            "country_event = { id = profile.1 title = profile_title set_country_flag = seen }\n";

        let hir = lower_with_profile(
            parse_eu4(Eu4FileFormat::PdxScript, source),
            &path,
            &rules,
            &profile(),
        );

        assert!(hir.definitions().iter().any(|definition| {
            definition.kind == "event"
                && definition.name == "profile.1"
                && definition.selection_range != definition.range
        }));
        assert!(
            hir.definitions()
                .iter()
                .any(|definition| definition.kind == "country_flag" && definition.name == "seen")
        );
        assert!(hir.references().iter().any(|reference| {
            reference.kind == "localisation" && reference.name == "profile_title"
        }));
    }

    #[test]
    fn identity_only_profile_does_not_create_game_specific_typed_facts() {
        let rules = bootstrap_rules();
        let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
        let source = "country_event = { id = profile.1 title = profile_title }\n";
        let profile = GameProfile::empty(rules.game_id());

        let hir = lower_with_profile(
            parse_eu4(Eu4FileFormat::PdxScript, source),
            &path,
            &rules,
            &profile,
        );

        assert!(hir.definitions().is_empty());
        assert!(!hir.references().iter().any(|reference| reference.kind == "localisation"));
    }
}
