//! Versioned serialization for source-independent scripted-macro template IR.

use pdx_text::TextRange;
use serde::{Deserialize, Serialize};

use crate::hir::{
    MacroTemplate, MacroTemplateConditional, MacroTemplateFragment, MacroTemplateItem,
    MacroTemplateProperty, MacroTemplateToken, MacroTemplateValue,
};

use super::{MAX_MACRO_TEMPLATE_BYTES, MAX_MACRO_TEMPLATE_NODES, VanillaCacheError};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(transparent)]
struct Range([u32; 2]);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Template {
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
    fn node(&mut self) -> Result<(), VanillaCacheError> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_MACRO_TEMPLATE_NODES {
            return Err(VanillaCacheError::LimitExceeded(
                "macro template node",
                MAX_MACRO_TEMPLATE_NODES,
            ));
        }
        Ok(())
    }

    fn text(&mut self, value: &str) -> Result<(), VanillaCacheError> {
        self.text_bytes = self.text_bytes.saturating_add(value.len());
        if self.text_bytes > MAX_MACRO_TEMPLATE_BYTES {
            return Err(VanillaCacheError::LimitExceeded(
                "macro template text byte",
                MAX_MACRO_TEMPLATE_BYTES,
            ));
        }
        Ok(())
    }
}

pub(super) fn encode(template: &MacroTemplate) -> Result<Vec<u8>, VanillaCacheError> {
    let dto = Template::from(template);
    let payload = serde_json::to_vec(&dto)
        .map_err(|error| VanillaCacheError::InvalidData(error.to_string()))?;
    if payload.len() > MAX_MACRO_TEMPLATE_BYTES {
        return Err(VanillaCacheError::LimitExceeded(
            "macro template byte",
            MAX_MACRO_TEMPLATE_BYTES,
        ));
    }
    Ok(payload)
}

pub(super) fn decode(
    payload: &[u8],
    expected_kind: &str,
    expected_name: &str,
    expected_definition_range: TextRange,
) -> Result<MacroTemplate, VanillaCacheError> {
    if payload.len() > MAX_MACRO_TEMPLATE_BYTES {
        return Err(VanillaCacheError::LimitExceeded(
            "macro template byte",
            MAX_MACRO_TEMPLATE_BYTES,
        ));
    }
    let dto: Template = serde_json::from_slice(payload).map_err(|error| {
        VanillaCacheError::InvalidData(format!("invalid macro template payload: {error}"))
    })?;
    let mut budget = Budget::default();
    let template = dto.into_model(&mut budget)?;
    if !template.kind.eq_ignore_ascii_case(expected_kind)
        || !template.name.eq_ignore_ascii_case(expected_name)
        || template.definition_range != expected_definition_range
    {
        return Err(VanillaCacheError::InvalidData(format!(
            "macro template identity does not match {expected_kind} `{expected_name}`"
        )));
    }
    validate_template_ranges(&template)?;
    Ok(template)
}

fn decode_range(range: Range) -> Result<TextRange, VanillaCacheError> {
    TextRange::new(range.0[0], range.0[1]).ok_or_else(|| {
        VanillaCacheError::InvalidData("macro template range end precedes start".to_owned())
    })
}

fn range_within(inner: TextRange, outer: TextRange) -> bool {
    inner.start() >= outer.start() && inner.end() <= outer.end()
}

fn validate_template_ranges(template: &MacroTemplate) -> Result<(), VanillaCacheError> {
    if !range_within(template.body_range, template.definition_range) {
        return Err(VanillaCacheError::InvalidData(
            "macro template body range escapes its definition".to_owned(),
        ));
    }
    validate_items_ranges(&template.items, template.definition_range)
}

fn validate_items_ranges(
    items: &[MacroTemplateItem],
    owner: TextRange,
) -> Result<(), VanillaCacheError> {
    for item in items {
        match item {
            MacroTemplateItem::Property(property) => {
                require_range(property.range, owner)?;
                validate_token_range(&property.key, owner)?;
                match &property.value {
                    MacroTemplateValue::Scalar(token) => validate_token_range(token, owner)?,
                    MacroTemplateValue::Block { range, items } => {
                        require_range(*range, owner)?;
                        validate_items_ranges(items, owner)?;
                    }
                }
            }
            MacroTemplateItem::BareValue(token) => validate_token_range(token, owner)?,
            MacroTemplateItem::Conditional(conditional) => {
                require_range(conditional.range, owner)?;
                validate_items_ranges(&conditional.items, owner)?;
            }
        }
    }
    Ok(())
}

fn validate_token_range(
    token: &MacroTemplateToken,
    owner: TextRange,
) -> Result<(), VanillaCacheError> {
    require_range(token.range, owner)?;
    for fragment in &token.fragments {
        if let MacroTemplateFragment::Parameter { range, .. } = fragment {
            require_range(*range, owner)?;
        }
    }
    Ok(())
}

fn require_range(range: TextRange, owner: TextRange) -> Result<(), VanillaCacheError> {
    if range_within(range, owner) {
        Ok(())
    } else {
        Err(VanillaCacheError::InvalidData(
            "macro template item range escapes its definition".to_owned(),
        ))
    }
}

impl From<&MacroTemplate> for Template {
    fn from(template: &MacroTemplate) -> Self {
        Self {
            kind: template.kind.clone(),
            name: template.name.clone(),
            definition_range: template.definition_range.into(),
            body_range: template.body_range.into(),
            items: template.items.iter().map(Item::from).collect(),
        }
    }
}

impl Template {
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplate, VanillaCacheError> {
        budget.node()?;
        budget.text(&self.kind)?;
        budget.text(&self.name)?;
        Ok(MacroTemplate {
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
) -> Result<Vec<MacroTemplateItem>, VanillaCacheError> {
    items
        .into_iter()
        .map(|item| item.into_model(budget))
        .collect()
}

impl Item {
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplateItem, VanillaCacheError> {
        budget.node()?;
        match self {
            Self::Property(property) => {
                Ok(MacroTemplateItem::Property(property.into_model(budget)?))
            }
            Self::BareValue(token) => Ok(MacroTemplateItem::BareValue(token.into_model(budget)?)),
            Self::Conditional(conditional) => Ok(MacroTemplateItem::Conditional(
                conditional.into_model(budget)?,
            )),
        }
    }
}

impl Property {
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplateProperty, VanillaCacheError> {
        if let Some(operator) = &self.operator {
            budget.text(operator)?;
        }
        Ok(MacroTemplateProperty {
            key: self.key.into_model(budget)?,
            range: decode_range(self.range)?,
            operator: self.operator,
            value: self.value.into_model(budget)?,
        })
    }
}

impl Value {
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplateValue, VanillaCacheError> {
        match self {
            Self::Scalar(token) => Ok(MacroTemplateValue::Scalar(token.into_model(budget)?)),
            Self::Block { range, items } => Ok(MacroTemplateValue::Block {
                range: decode_range(range)?,
                items: decode_items(items, budget)?,
            }),
        }
    }
}

impl Conditional {
    fn into_model(
        self,
        budget: &mut Budget,
    ) -> Result<MacroTemplateConditional, VanillaCacheError> {
        budget.text(&self.name)?;
        Ok(MacroTemplateConditional {
            name: self.name,
            negated: self.negated,
            range: decode_range(self.range)?,
            items: decode_items(self.items, budget)?,
        })
    }
}

impl Token {
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplateToken, VanillaCacheError> {
        Ok(MacroTemplateToken {
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
    fn into_model(self, budget: &mut Budget) -> Result<MacroTemplateFragment, VanillaCacheError> {
        budget.node()?;
        match self {
            Self::Literal(value) => {
                budget.text(&value)?;
                Ok(MacroTemplateFragment::Literal(value))
            }
            Self::Parameter { name, range } => {
                budget.text(&name)?;
                Ok(MacroTemplateFragment::Parameter {
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

impl From<&MacroTemplateItem> for Item {
    fn from(item: &MacroTemplateItem) -> Self {
        match item {
            MacroTemplateItem::Property(property) => Self::Property(property.into()),
            MacroTemplateItem::BareValue(token) => Self::BareValue(token.into()),
            MacroTemplateItem::Conditional(conditional) => Self::Conditional(conditional.into()),
        }
    }
}

impl From<&MacroTemplateProperty> for Property {
    fn from(property: &MacroTemplateProperty) -> Self {
        Self {
            key: (&property.key).into(),
            range: property.range.into(),
            operator: property.operator.clone(),
            value: (&property.value).into(),
        }
    }
}

impl From<&MacroTemplateValue> for Value {
    fn from(value: &MacroTemplateValue) -> Self {
        match value {
            MacroTemplateValue::Scalar(token) => Self::Scalar(token.into()),
            MacroTemplateValue::Block { range, items } => Self::Block {
                range: (*range).into(),
                items: items.iter().map(Item::from).collect(),
            },
        }
    }
}

impl From<&MacroTemplateConditional> for Conditional {
    fn from(conditional: &MacroTemplateConditional) -> Self {
        Self {
            name: conditional.name.clone(),
            negated: conditional.negated,
            range: conditional.range.into(),
            items: conditional.items.iter().map(Item::from).collect(),
        }
    }
}

impl From<&MacroTemplateToken> for Token {
    fn from(token: &MacroTemplateToken) -> Self {
        Self {
            range: token.range.into(),
            quoted: token.quoted,
            fragments: token.fragments.iter().map(Fragment::from).collect(),
        }
    }
}

impl From<&MacroTemplateFragment> for Fragment {
    fn from(fragment: &MacroTemplateFragment) -> Self {
        match fragment {
            MacroTemplateFragment::Literal(value) => Self::Literal(value.clone()),
            MacroTemplateFragment::Parameter { name, range } => Self::Parameter {
                name: name.clone(),
                range: (*range).into(),
            },
        }
    }
}
