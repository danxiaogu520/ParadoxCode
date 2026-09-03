//! Ordered, source-ranged dynamic-definition templates.

use pdx_parser::{CstKind, CstNode, ParsedFile};
use pdx_rules::RuleSet;
use pdx_text::TextRange;

use super::{
    HirDefinition, HirParameterConditional, HirParameterReference, Template, TemplateConditional,
    TemplateFragment, TemplateItem, TemplateProperty, TemplateToken, TemplateValue, range_within,
};

pub(super) fn lower_dynamic_templates(
    syntax: &ParsedFile,
    definitions: &[HirDefinition],
    conditionals: &[HirParameterConditional],
    references: &[HirParameterReference],
    rules: &RuleSet,
) -> Vec<Template> {
    let mut templates = Vec::new();
    for definition in definitions {
        let enabled = rules
            .model()
            .semantic
            .type_descriptors
            .iter()
            .find(|(kind, _)| kind.eq_ignore_ascii_case(&definition.kind))
            .and_then(|(_, descriptor)| descriptor.dynamic_definition.as_ref())
            .is_some_and(|descriptor| descriptor.enabled);
        if !enabled
            || syntax.errors().iter().any(|error| {
                error.range.start() >= definition.range.start()
                    && error.range.end() <= definition.range.end()
            })
        {
            continue;
        }
        let Some(owner) = find_node(syntax.root(), CstKind::Property, definition.range) else {
            continue;
        };
        let Some(block) = property_value_child(owner, CstKind::Block) else {
            continue;
        };
        let owner_references = references
            .iter()
            .filter(|reference| reference.owner_range == definition.range)
            .collect::<Vec<_>>();
        let Some(items) = template_items(syntax, block.children(), conditionals, &owner_references)
        else {
            continue;
        };
        templates.push(Template {
            kind: definition.kind.clone(),
            name: definition.name.clone(),
            definition_range: definition.range,
            body_range: block.range(),
            items,
        });
    }
    templates.sort_by_key(|template| template.definition_range.start());
    templates
}

fn template_items<'t>(
    syntax: &ParsedFile,
    nodes: impl Iterator<Item = CstNode<'t>>,
    conditionals: &[HirParameterConditional],
    references: &[&HirParameterReference],
) -> Option<Vec<TemplateItem>> {
    let mut items = Vec::new();
    for node in nodes {
        match node.kind() {
            CstKind::Property => items.push(TemplateItem::Property(template_property(
                syntax,
                node,
                conditionals,
                references,
            )?)),
            CstKind::BareValue | CstKind::QuotedString => {
                items.push(TemplateItem::BareValue(template_token(
                    syntax, node, references,
                )?));
            }
            CstKind::ParameterBlock => {
                let conditional = conditionals
                    .iter()
                    .find(|conditional| conditional.range == node.range())?;
                let body = node
                    .children()
                    .filter(|child| child.kind() != CstKind::ParameterCondition)
                    .collect::<Vec<_>>();
                items.push(TemplateItem::Conditional(TemplateConditional {
                    name: conditional.name.clone(),
                    negated: conditional.negated,
                    range: conditional.range,
                    items: template_items(syntax, body.iter().copied(), conditionals, references)?,
                }));
            }
            CstKind::Comment | CstKind::Bom => {}
            CstKind::Error
            | CstKind::HeaderBlock
            | CstKind::Document
            | CstKind::Key
            | CstKind::Operator
            | CstKind::Value
            | CstKind::Block
            | CstKind::ParameterCondition
            | CstKind::LocalisationDocument
            | CstKind::LanguageHeader
            | CstKind::LocalisationEntry
            | CstKind::LocalisationKey
            | CstKind::Version
            | CstKind::LocalisationString
            | CstKind::UnquotedValue => return None,
        }
    }
    Some(items)
}

fn template_property(
    syntax: &ParsedFile,
    node: CstNode<'_>,
    conditionals: &[HirParameterConditional],
    references: &[&HirParameterReference],
) -> Option<TemplateProperty> {
    let key = node.children().find(|child| child.kind() == CstKind::Key)?;
    let operator = node
        .children()
        .find(|child| child.kind() == CstKind::Operator)
        .and_then(|operator| syntax.text(operator.range()))
        .map(str::trim)
        .filter(|operator| !operator.is_empty())
        .map(str::to_owned);
    let value = node
        .children()
        .find(|child| child.kind() == CstKind::Value)?
        .children()
        .next()?;
    let value = match value.kind() {
        CstKind::BareValue | CstKind::QuotedString => {
            TemplateValue::Scalar(template_token(syntax, value, references)?)
        }
        CstKind::Block => TemplateValue::Block {
            range: value.range(),
            items: template_items(syntax, value.children(), conditionals, references)?,
        },
        _ => return None,
    };
    Some(TemplateProperty {
        key: template_token(syntax, key, references)?,
        range: node.range(),
        operator,
        value,
    })
}

fn template_token(
    syntax: &ParsedFile,
    node: CstNode<'_>,
    references: &[&HirParameterReference],
) -> Option<TemplateToken> {
    let raw = syntax.text(node.range())?;
    let quoted = raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2;
    let content_range = if quoted {
        TextRange::new(
            node.range().start().saturating_add(1),
            node.range().end().saturating_sub(1),
        )?
    } else {
        node.range()
    };
    let mut slots = references
        .iter()
        .copied()
        .filter(|reference| range_within(reference.range, content_range))
        .collect::<Vec<_>>();
    slots.sort_by_key(|reference| reference.range.start());
    let mut fragments = Vec::new();
    let mut cursor = content_range.start();
    for reference in slots {
        if reference.range.start() < cursor {
            continue;
        }
        if cursor < reference.range.start() {
            fragments.push(TemplateFragment::Literal(
                syntax
                    .text(TextRange::new(cursor, reference.range.start())?)?
                    .to_owned(),
            ));
        }
        fragments.push(TemplateFragment::Parameter {
            name: reference.name.clone(),
            range: reference.range,
        });
        cursor = reference.range.end();
    }
    if cursor < content_range.end() {
        fragments.push(TemplateFragment::Literal(
            syntax
                .text(TextRange::new(cursor, content_range.end())?)?
                .to_owned(),
        ));
    }
    if fragments.is_empty() {
        fragments.push(TemplateFragment::Literal(
            syntax.text(content_range)?.to_owned(),
        ));
    }
    Some(TemplateToken {
        range: node.range(),
        quoted,
        fragments,
    })
}

fn property_value_child(node: CstNode<'_>, kind: CstKind) -> Option<CstNode<'_>> {
    node.children()
        .find(|child| child.kind() == CstKind::Value)?
        .children()
        .find(|child| child.kind() == kind)
}

fn find_node(node: CstNode<'_>, kind: CstKind, range: TextRange) -> Option<CstNode<'_>> {
    if node.kind() == kind && node.range() == range {
        return Some(node);
    }
    node.children()
        .find_map(|child| find_node(child, kind, range))
}
