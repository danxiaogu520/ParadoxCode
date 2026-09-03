//! Versioned serialization for source-independent scripted-dynamic template IR.

use pdx_text::TextRange;
use serde::{Deserialize, Serialize};

use crate::hir::{
    Template, TemplateConditional, TemplateFragment, TemplateItem, TemplateProperty, TemplateToken,
    TemplateValue,
};

use super::{IndexCacheError, MAX_TEMPLATE_BYTES, MAX_TEMPLATE_NODES};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(transparent)]
struct Range([u32; 2]);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EncodedTemplate {
    kind: String,
    name: String,
    definition_range: Range,
    body_range: Range,
    items: Vec<Item>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Item {
    Property(Property),
    BareValue(Token),
    Conditional(Conditional),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Property {
    key: Token,
    range: Range,
    operator: Option<String>,
    value: Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Value {
    Scalar(Token),
    Block { range: Range, items: Vec<Item> },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Conditional {
    name: String,
    negated: bool,
    range: Range,
    items: Vec<Item>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Token {
    range: Range,
    quoted: bool,
    fragments: Vec<Fragment>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Fragment {
    Literal(String),
    Parameter { name: String, range: Range },
}

#[derive(Default)]
struct Budget {
    nodes: usize,
    text_bytes: usize,
}

impl Budget {
    fn node(&mut self) -> Result<(), IndexCacheError> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_TEMPLATE_NODES {
            return Err(IndexCacheError::LimitExceeded(
                "dynamic template node",
                MAX_TEMPLATE_NODES,
            ));
        }
        Ok(())
    }

    fn text(&mut self, value: &str) -> Result<(), IndexCacheError> {
        self.text_bytes = self.text_bytes.saturating_add(value.len());
        if self.text_bytes > MAX_TEMPLATE_BYTES {
            return Err(IndexCacheError::LimitExceeded(
                "dynamic template text byte",
                MAX_TEMPLATE_BYTES,
            ));
        }
        Ok(())
    }
}

pub(super) fn encode(template: &Template) -> Result<Vec<u8>, IndexCacheError> {
    let dto = EncodedTemplate::from(template);
    let payload = serde_json::to_vec(&dto)
        .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?;
    if payload.len() > MAX_TEMPLATE_BYTES {
        return Err(IndexCacheError::LimitExceeded(
            "dynamic template byte",
            MAX_TEMPLATE_BYTES,
        ));
    }
    Ok(payload)
}

pub(super) fn decode(
    payload: &[u8],
    expected_kind: &str,
    expected_name: &str,
    expected_definition_range: TextRange,
) -> Result<Template, IndexCacheError> {
    if payload.len() > MAX_TEMPLATE_BYTES {
        return Err(IndexCacheError::LimitExceeded(
            "dynamic template byte",
            MAX_TEMPLATE_BYTES,
        ));
    }
    let dto: EncodedTemplate = serde_json::from_slice(payload).map_err(|error| {
        IndexCacheError::InvalidData(format!("invalid dynamic template payload: {error}"))
    })?;
    let mut budget = Budget::default();
    let template = dto.into_model(&mut budget)?;
    if !template.kind.eq_ignore_ascii_case(expected_kind)
        || !template.name.eq_ignore_ascii_case(expected_name)
        || template.definition_range != expected_definition_range
    {
        return Err(IndexCacheError::InvalidData(format!(
            "dynamic template identity does not match {expected_kind} `{expected_name}`"
        )));
    }
    validate_template_ranges(&template)?;
    Ok(template)
}

fn decode_range(range: Range) -> Result<TextRange, IndexCacheError> {
    TextRange::new(range.0[0], range.0[1]).ok_or_else(|| {
        IndexCacheError::InvalidData("dynamic template range end precedes start".to_owned())
    })
}

fn range_within(inner: TextRange, outer: TextRange) -> bool {
    inner.start() >= outer.start() && inner.end() <= outer.end()
}

fn validate_template_ranges(template: &Template) -> Result<(), IndexCacheError> {
    if !range_within(template.body_range, template.definition_range) {
        return Err(IndexCacheError::InvalidData(
            "dynamic template body range escapes its definition".to_owned(),
        ));
    }
    validate_items_ranges(&template.items, template.definition_range)
}

fn validate_items_ranges(items: &[TemplateItem], owner: TextRange) -> Result<(), IndexCacheError> {
    for item in items {
        match item {
            TemplateItem::Property(property) => {
                require_range(property.range, owner)?;
                validate_token_range(&property.key, owner)?;
                match &property.value {
                    TemplateValue::Scalar(token) => validate_token_range(token, owner)?,
                    TemplateValue::Block { range, items } => {
                        require_range(*range, owner)?;
                        validate_items_ranges(items, owner)?;
                    }
                }
            }
            TemplateItem::BareValue(token) => validate_token_range(token, owner)?,
            TemplateItem::Conditional(conditional) => {
                require_range(conditional.range, owner)?;
                validate_items_ranges(&conditional.items, owner)?;
            }
        }
    }
    Ok(())
}

fn validate_token_range(token: &TemplateToken, owner: TextRange) -> Result<(), IndexCacheError> {
    require_range(token.range, owner)?;
    for fragment in &token.fragments {
        if let TemplateFragment::Parameter { range, .. } = fragment {
            require_range(*range, owner)?;
        }
    }
    Ok(())
}

fn require_range(range: TextRange, owner: TextRange) -> Result<(), IndexCacheError> {
    if range_within(range, owner) {
        Ok(())
    } else {
        Err(IndexCacheError::InvalidData(
            "dynamic template item range escapes its definition".to_owned(),
        ))
    }
}

impl From<&Template> for EncodedTemplate {
    fn from(template: &Template) -> Self {
        Self {
            kind: template.kind.clone(),
            name: template.name.clone(),
            definition_range: template.definition_range.into(),
            body_range: template.body_range.into(),
            items: template.items.iter().map(Item::from).collect(),
        }
    }
}

impl EncodedTemplate {
    fn into_model(self, budget: &mut Budget) -> Result<Template, IndexCacheError> {
        budget.node()?;
        budget.text(&self.kind)?;
        budget.text(&self.name)?;
        Ok(Template {
            kind: self.kind,
            name: self.name,
            definition_range: decode_range(self.definition_range)?,
            body_range: decode_range(self.body_range)?,
            items: decode_items(self.items, budget)?,
        })
    }
}

fn decode_items(
    items: Vec<Item>,
    budget: &mut Budget,
) -> Result<Vec<TemplateItem>, IndexCacheError> {
    items
        .into_iter()
        .map(|item| item.into_model(budget))
        .collect()
}

impl Item {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateItem, IndexCacheError> {
        budget.node()?;
        match self {
            Self::Property(property) => Ok(TemplateItem::Property(property.into_model(budget)?)),
            Self::BareValue(token) => Ok(TemplateItem::BareValue(token.into_model(budget)?)),
            Self::Conditional(conditional) => {
                Ok(TemplateItem::Conditional(conditional.into_model(budget)?))
            }
        }
    }
}

impl Property {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateProperty, IndexCacheError> {
        if let Some(operator) = &self.operator {
            budget.text(operator)?;
        }
        Ok(TemplateProperty {
            key: self.key.into_model(budget)?,
            range: decode_range(self.range)?,
            operator: self.operator,
            value: self.value.into_model(budget)?,
        })
    }
}

impl Value {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateValue, IndexCacheError> {
        match self {
            Self::Scalar(token) => Ok(TemplateValue::Scalar(token.into_model(budget)?)),
            Self::Block { range, items } => Ok(TemplateValue::Block {
                range: decode_range(range)?,
                items: decode_items(items, budget)?,
            }),
        }
    }
}

impl Conditional {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateConditional, IndexCacheError> {
        budget.text(&self.name)?;
        Ok(TemplateConditional {
            name: self.name,
            negated: self.negated,
            range: decode_range(self.range)?,
            items: decode_items(self.items, budget)?,
        })
    }
}

impl Token {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateToken, IndexCacheError> {
        Ok(TemplateToken {
            range: decode_range(self.range)?,
            quoted: self.quoted,
            fragments: self
                .fragments
                .into_iter()
                .map(|fragment| fragment.into_model(budget))
                .collect::<Result<_, _>>()?,
        })
    }
}

impl Fragment {
    fn into_model(self, budget: &mut Budget) -> Result<TemplateFragment, IndexCacheError> {
        budget.node()?;
        match self {
            Self::Literal(value) => {
                budget.text(&value)?;
                Ok(TemplateFragment::Literal(value))
            }
            Self::Parameter { name, range } => {
                budget.text(&name)?;
                Ok(TemplateFragment::Parameter {
                    name,
                    range: decode_range(range)?,
                })
            }
        }
    }
}

impl From<TextRange> for Range {
    fn from(range: TextRange) -> Self {
        Self([range.start(), range.end()])
    }
}

impl From<&TemplateItem> for Item {
    fn from(item: &TemplateItem) -> Self {
        match item {
            TemplateItem::Property(property) => Self::Property(property.into()),
            TemplateItem::BareValue(token) => Self::BareValue(token.into()),
            TemplateItem::Conditional(conditional) => Self::Conditional(conditional.into()),
        }
    }
}

impl From<&TemplateProperty> for Property {
    fn from(property: &TemplateProperty) -> Self {
        Self {
            key: (&property.key).into(),
            range: property.range.into(),
            operator: property.operator.clone(),
            value: (&property.value).into(),
        }
    }
}

impl From<&TemplateValue> for Value {
    fn from(value: &TemplateValue) -> Self {
        match value {
            TemplateValue::Scalar(token) => Self::Scalar(token.into()),
            TemplateValue::Block { range, items } => Self::Block {
                range: (*range).into(),
                items: items.iter().map(Item::from).collect(),
            },
        }
    }
}

impl From<&TemplateConditional> for Conditional {
    fn from(conditional: &TemplateConditional) -> Self {
        Self {
            name: conditional.name.clone(),
            negated: conditional.negated,
            range: conditional.range.into(),
            items: conditional.items.iter().map(Item::from).collect(),
        }
    }
}

impl From<&TemplateToken> for Token {
    fn from(token: &TemplateToken) -> Self {
        Self {
            range: token.range.into(),
            quoted: token.quoted,
            fragments: token.fragments.iter().map(Fragment::from).collect(),
        }
    }
}

impl From<&TemplateFragment> for Fragment {
    fn from(fragment: &TemplateFragment) -> Self {
        match fragment {
            TemplateFragment::Literal(value) => Self::Literal(value.clone()),
            TemplateFragment::Parameter { name, range } => Self::Parameter {
                name: name.clone(),
                range: (*range).into(),
            },
        }
    }
}
