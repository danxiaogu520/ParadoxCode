//! Loss-aware CST fact collection.

use pdx_parser::{CstKind, CstNode, ParsedFile};
use pdx_text::TextRange;

use super::{
    HirLocalisationEntry, HirParameterConditional, HirProperty, HirScalar, HirUnknownConstruct,
};

pub(super) struct CollectedFacts {
    pub(super) properties: Vec<HirProperty>,
    pub(super) localisation_entries: Vec<HirLocalisationEntry>,
    pub(super) bare_values: Vec<HirScalar>,
    pub(super) unknown_constructs: Vec<HirUnknownConstruct>,
    pub(super) parameter_conditionals: Vec<HirParameterConditional>,
}

struct FactCollector<'syntax> {
    syntax: &'syntax ParsedFile,
    properties: Vec<HirProperty>,
    localisation_entries: Vec<HirLocalisationEntry>,
    bare_values: Vec<HirScalar>,
    unknown_constructs: Vec<HirUnknownConstruct>,
    parameter_conditionals: Vec<HirParameterConditional>,
}

impl<'syntax> FactCollector<'syntax> {
    fn new(syntax: &'syntax ParsedFile) -> Self {
        Self {
            syntax,
            properties: Vec::new(),
            localisation_entries: Vec::new(),
            bare_values: Vec::new(),
            unknown_constructs: Vec::new(),
            parameter_conditionals: Vec::new(),
        }
    }

    fn collect(
        &mut self,
        node: &CstNode,
        top_level: bool,
        inside_key: bool,
        parent_path: &[String],
    ) {
        if node.kind() == CstKind::Error {
            self.unknown_constructs.push(HirUnknownConstruct {
                range: node.range(),
            });
        }
        if node.kind() == CstKind::ParameterBlock
            && let Some(condition) = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::ParameterCondition)
            && let Some(raw) = self.syntax.text(condition.range()).map(str::trim)
        {
            let (negated, name) = raw
                .strip_prefix('!')
                .map_or((false, raw), |name| (true, name));
            if !name.is_empty() {
                let raw_text = self.syntax.text(condition.range()).unwrap_or_default();
                let name_offset = raw_text.find(name).unwrap_or(0);
                let start = condition
                    .range()
                    .start()
                    .saturating_add(u32::try_from(name_offset).unwrap_or(0));
                let end = start.saturating_add(u32::try_from(name.len()).unwrap_or(0));
                self.parameter_conditionals.push(HirParameterConditional {
                    name: name.to_owned(),
                    negated,
                    range: node.range(),
                    condition_range: condition.range(),
                    name_range: TextRange::new(start, end).unwrap_or(condition.range()),
                });
            }
        }
        if node.kind() == CstKind::LocalisationEntry
            && let Some(key) = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::LocalisationKey)
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
            && let Some(value) = self
                .syntax
                .text(node.range())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            self.bare_values.push(HirScalar {
                value: value.to_owned(),
                range: node.range(),
            });
        }
        if node.kind() == CstKind::Property
            && let Some(key_node) = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Key)
            && let Some(key) = self
                .syntax
                .text(key_node.range())
                .map(str::trim)
                .filter(|key| !key.is_empty())
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
                self.collect(
                    child,
                    false,
                    inside_key || node.kind() == CstKind::Key,
                    &path,
                );
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
    let value = node
        .children()
        .iter()
        .find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .iter()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let raw = syntax.text(scalar.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw)
        .to_owned();
    Some(HirScalar {
        value,
        range: scalar.range(),
    })
}

pub(super) fn collect(syntax: &ParsedFile) -> CollectedFacts {
    let mut collector = FactCollector::new(syntax);
    collector.collect(syntax.root(), true, false, &[]);
    CollectedFacts {
        properties: collector.properties,
        localisation_entries: collector.localisation_entries,
        bare_values: collector.bare_values,
        unknown_constructs: collector.unknown_constructs,
        parameter_conditionals: collector.parameter_conditionals,
    }
}
