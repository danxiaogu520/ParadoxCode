//! Rule-aware, game-independent semantic lowering boundary.

use std::sync::Arc;

use pdx_rules::RuleSet;
use pdx_syntax::{CstKind, CstNode, ParsedFile};
use pdx_text::TextRange;

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

/// A lowered file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFile {
    syntax: Arc<ParsedFile>,
    scope: Scope,
    properties: Vec<HirProperty>,
    localisation_entries: Vec<HirLocalisationEntry>,
    bare_values: Vec<HirScalar>,
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
}

/// Lowers a parsed PDX file into game-independent structural facts.
#[must_use]
pub fn lower(syntax: ParsedFile, rules: &RuleSet) -> HirFile {
    lower_shared(Arc::new(syntax), rules)
}

/// Lowers a shared parsed file without copying its CST.
#[must_use]
pub fn lower_shared(syntax: Arc<ParsedFile>, _rules: &RuleSet) -> HirFile {
    let (properties, localisation_entries, bare_values) = {
        let mut collector = FactCollector::new(&syntax);
        collector.collect(syntax.root(), true, false, &[]);
        (collector.properties, collector.localisation_entries, collector.bare_values)
    };
    HirFile { syntax, scope: Scope::Unknown, properties, localisation_entries, bare_values }
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
    use super::lower;
    use pdx_rules::RuleSet;
    use pdx_syntax::{Eu4FileFormat, parse_eu4};

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
}
