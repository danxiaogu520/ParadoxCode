//! Query-local scripted-macro binding and tree instantiation.

use std::collections::BTreeMap;
use std::sync::Arc;

use pdx_engine::hir::{
    MacroTemplate, MacroTemplateFragment, MacroTemplateItem, MacroTemplateProperty,
    MacroTemplateToken, MacroTemplateValue,
};
use pdx_text::TextRange;

use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::semantic::{MacroDefinitionIdentity, ResolvedMacroDefinition};
use crate::support::{QuotedScalarSource, ScriptProperty, quoted_script_container};
use crate::types::{CancellationToken, Cancelled};

const MAX_EXPANSION_DEPTH: usize = 32;
const MAX_EXPANDED_NODES: usize = 50_000;
const MAX_EXPANDED_TOKEN_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct ExpansionFrame {
    identity: MacroDefinitionIdentity,
    name: String,
}

#[derive(Clone, Debug)]
enum BoundValue {
    Scalar {
        value: String,
        range: TextRange,
        quoted_source: Option<QuotedScalarSource>,
    },
    Invalid {
        range: TextRange,
    },
}

#[derive(Clone, Debug)]
struct RenderedToken {
    value: String,
    range: TextRange,
    quoted_source: Option<QuotedScalarSource>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExpandedContainer {
    pub(crate) properties: Vec<ScriptProperty>,
    pub(crate) bare_values: Vec<(String, TextRange)>,
    omitted_optional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpansionFailure {
    MissingParameter(String),
    InvalidArgument { name: String, range: TextRange },
    OmitOptionalProperty,
    Limit(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpansionEnterFailure {
    Cycle(Vec<String>),
    Limit(&'static str),
}

#[derive(Debug, Default)]
pub(crate) struct MacroExpansionSession {
    stack: Vec<ExpansionFrame>,
    expanded_nodes: usize,
    expanded_token_bytes: usize,
    limit_reported: bool,
}

impl MacroExpansionSession {
    pub(crate) fn enter(
        &mut self,
        resolved: &ResolvedMacroDefinition,
    ) -> Result<(), ExpansionEnterFailure> {
        if let Some(cycle_start) = self
            .stack
            .iter()
            .position(|frame| frame.identity == resolved.identity)
        {
            let mut chain = self.stack[cycle_start..]
                .iter()
                .map(|frame| frame.name.clone())
                .collect::<Vec<_>>();
            chain.push(resolved.summary.name.clone());
            return Err(ExpansionEnterFailure::Cycle(chain));
        }
        if self.stack.len() >= MAX_EXPANSION_DEPTH {
            return Err(ExpansionEnterFailure::Limit("expansion depth"));
        }
        self.stack.push(ExpansionFrame {
            identity: resolved.identity.clone(),
            name: resolved.summary.name.clone(),
        });
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        let _ = self.stack.pop();
    }

    pub(crate) fn expand(
        &mut self,
        template: &MacroTemplate,
        invocation: &ScriptProperty,
        cancellation: &CancellationToken,
        quoted_scripts: &mut QuotedScriptSession<'_>,
        quoted_script_depth: usize,
    ) -> Result<Result<ExpandedContainer, ExpansionFailure>, Cancelled> {
        let bindings = bind_arguments(invocation);
        self.expand_items(
            &template.items,
            &bindings,
            invocation.key_range,
            cancellation,
            quoted_scripts,
            quoted_script_depth,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_items(
        &mut self,
        items: &[MacroTemplateItem],
        bindings: &BTreeMap<String, BoundValue>,
        fallback_range: TextRange,
        cancellation: &CancellationToken,
        quoted_scripts: &mut QuotedScriptSession<'_>,
        quoted_script_depth: usize,
        runtime_guarded: bool,
    ) -> Result<Result<ExpandedContainer, ExpansionFailure>, Cancelled> {
        let mut expanded = ExpandedContainer::default();
        for item in items {
            cancellation.checkpoint()?;
            if let Err(error) = self.charge_node() {
                return Ok(Err(error));
            }
            match item {
                MacroTemplateItem::Property(property) => {
                    let property_guarded = runtime_guarded && !is_limit_property(property);
                    let property = match self.expand_property(
                        property,
                        bindings,
                        fallback_range,
                        cancellation,
                        quoted_scripts,
                        quoted_script_depth,
                        property_guarded,
                    )? {
                        Ok(property) => property,
                        Err(ExpansionFailure::OmitOptionalProperty) => {
                            if !is_runtime_branch_property(property) {
                                expanded.omitted_optional = true;
                            }
                            continue;
                        }
                        Err(error) => return Ok(Err(error)),
                    };
                    if let Some(property) = property {
                        expanded.properties.push(property);
                    }
                }
                MacroTemplateItem::BareValue(token) => {
                    if runtime_guarded && missing_single_parameter(token, bindings) {
                        expanded.omitted_optional = true;
                        continue;
                    }
                    let rendered = match self.render_token(token, bindings, fallback_range) {
                        Ok(value) => value,
                        Err(error) => return Ok(Err(error)),
                    };
                    if let Some(origin) = rendered.quoted_source.as_ref() {
                        match quoted_scripts.parse(origin.source(), quoted_script_depth)? {
                            QuotedScriptParse::Parsed(script) => {
                                let (properties, bare_values) =
                                    quoted_script_container(&script, origin);
                                for _ in 0..properties.len().saturating_add(bare_values.len()) {
                                    if let Err(error) = self.charge_node() {
                                        return Ok(Err(error));
                                    }
                                }
                                expanded.properties.extend(properties);
                                expanded.bare_values.extend(bare_values);
                            }
                            QuotedScriptParse::Opaque => {
                                expanded.bare_values.push((rendered.value, rendered.range))
                            }
                            QuotedScriptParse::Limited(limit) => {
                                return Ok(Err(ExpansionFailure::Limit(limit.message())));
                            }
                        }
                    } else {
                        expanded.bare_values.push((rendered.value, rendered.range));
                    }
                }
                MacroTemplateItem::Conditional(conditional) => {
                    let supplied = bindings.contains_key(&conditional.name.to_ascii_lowercase());
                    if supplied != conditional.negated {
                        let nested = match self.expand_items(
                            &conditional.items,
                            bindings,
                            fallback_range,
                            cancellation,
                            quoted_scripts,
                            quoted_script_depth,
                            runtime_guarded,
                        )? {
                            Ok(nested) => nested,
                            Err(error) => return Ok(Err(error)),
                        };
                        expanded.properties.extend(nested.properties);
                        expanded.bare_values.extend(nested.bare_values);
                        expanded.omitted_optional |= nested.omitted_optional;
                    }
                }
            }
        }
        Ok(Ok(expanded))
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_property(
        &mut self,
        property: &MacroTemplateProperty,
        bindings: &BTreeMap<String, BoundValue>,
        fallback_range: TextRange,
        cancellation: &CancellationToken,
        quoted_scripts: &mut QuotedScriptSession<'_>,
        quoted_script_depth: usize,
        runtime_guarded: bool,
    ) -> Result<Result<Option<ScriptProperty>, ExpansionFailure>, Cancelled> {
        let forwarding = is_forwarding_property(property, bindings);
        if missing_single_parameter(&property.key, bindings) && (runtime_guarded || forwarding) {
            // A key substitution with no binding cannot produce a meaningful property. The
            // signature validator has already rejected missing required parameters; this path
            // is for optional values that live in a runtime branch of the macro body.
            return Ok(Err(ExpansionFailure::OmitOptionalProperty));
        }
        let mut key = match self.render_token(&property.key, bindings, fallback_range) {
            Ok(key) => key,
            Err(error) => return Ok(Err(error)),
        };
        key.value = key.value.trim().to_owned();
        if let MacroTemplateValue::Scalar(token) = &property.value
            && missing_single_parameter(token, bindings)
            && (runtime_guarded || forwarding)
        {
            // EU4 macros commonly place optional substitutions in branch-local scalar values.
            // Omitting that property preserves the surrounding definition and lets sibling
            // branches validate normally.
            return Ok(Err(ExpansionFailure::OmitOptionalProperty));
        }
        let (scalar, quoted, quoted_source, block_range, block, bare_values) = match &property.value
        {
            MacroTemplateValue::Scalar(token) => {
                let scalar = match self.render_token(token, bindings, fallback_range) {
                    Ok(scalar) => scalar,
                    Err(error) => return Ok(Err(error)),
                };
                let quoted = scalar.quoted_source.is_some();
                (
                    Some((scalar.value, scalar.range)),
                    quoted,
                    scalar.quoted_source,
                    None,
                    Vec::new(),
                    Vec::new(),
                )
            }
            MacroTemplateValue::Block { items, .. } => {
                let nested = match self.expand_items(
                    items,
                    bindings,
                    fallback_range,
                    cancellation,
                    quoted_scripts,
                    quoted_script_depth,
                    runtime_guarded || is_runtime_branch_key(&key.value),
                )? {
                    Ok(nested) => nested,
                    Err(error) => return Ok(Err(error)),
                };
                if nested.omitted_optional {
                    return Ok(Err(ExpansionFailure::OmitOptionalProperty));
                }
                (
                    None,
                    false,
                    None,
                    Some(fallback_range),
                    nested.properties,
                    nested.bare_values,
                )
            }
        };
        Ok(Ok(Some(ScriptProperty {
            key: key.value,
            key_range: key.range,
            range: fallback_range,
            operator: property.operator.clone(),
            scalar,
            quoted,
            quoted_source,
            block_range,
            block,
            bare_values,
        })))
    }

    fn render_token(
        &mut self,
        token: &MacroTemplateToken,
        bindings: &BTreeMap<String, BoundValue>,
        fallback_range: TextRange,
    ) -> Result<RenderedToken, ExpansionFailure> {
        let mut value = String::new();
        let mut origin = fallback_range;
        let mut parameter_origin = false;
        let mut quoted_source = None;
        for fragment in &token.fragments {
            match fragment {
                MacroTemplateFragment::Literal(literal) => value.push_str(literal),
                MacroTemplateFragment::Parameter { name, .. } => {
                    let Some(binding) = bindings.get(&name.to_ascii_lowercase()) else {
                        if token.fragments.len() > 1 {
                            continue;
                        }
                        return Err(ExpansionFailure::MissingParameter(name.clone()));
                    };
                    match binding {
                        BoundValue::Scalar {
                            value: argument,
                            range,
                            quoted_source: argument_source,
                        } => {
                            value.push_str(argument);
                            if !parameter_origin {
                                origin = *range;
                                parameter_origin = true;
                            }
                            if token.fragments.len() == 1 {
                                quoted_source.clone_from(argument_source);
                            }
                        }
                        BoundValue::Invalid { range } => {
                            return Err(ExpansionFailure::InvalidArgument {
                                name: name.clone(),
                                range: *range,
                            });
                        }
                    }
                }
            }
        }
        self.charge_token_bytes(value.len())?;
        if quoted_source.is_none() && token.quoted {
            let source = Arc::<str>::from(format!("\"{value}\""));
            quoted_source = Some(QuotedScalarSource::synthetic(source, origin));
        }
        Ok(RenderedToken {
            value,
            range: origin,
            quoted_source,
        })
    }

    pub(crate) fn charge_node(&mut self) -> Result<(), ExpansionFailure> {
        self.expanded_nodes = self.expanded_nodes.saturating_add(1);
        if self.expanded_nodes > MAX_EXPANDED_NODES {
            Err(ExpansionFailure::Limit("expanded nodes"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn charge_token_bytes(&mut self, bytes: usize) -> Result<(), ExpansionFailure> {
        self.expanded_token_bytes = self.expanded_token_bytes.saturating_add(bytes);
        if self.expanded_token_bytes > MAX_EXPANDED_TOKEN_BYTES {
            Err(ExpansionFailure::Limit("expanded token bytes"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn should_report_limit(&mut self) -> bool {
        if self.limit_reported {
            false
        } else {
            self.limit_reported = true;
            true
        }
    }
}

fn missing_single_parameter(
    token: &MacroTemplateToken,
    bindings: &BTreeMap<String, BoundValue>,
) -> bool {
    matches!(
        token.fragments.as_slice(),
        [MacroTemplateFragment::Parameter { name, .. }]
            if !bindings.contains_key(&name.to_ascii_lowercase())
    )
}

fn is_forwarding_property(
    property: &MacroTemplateProperty,
    bindings: &BTreeMap<String, BoundValue>,
) -> bool {
    let MacroTemplateValue::Scalar(value) = &property.value else {
        return false;
    };
    let [MacroTemplateFragment::Parameter { name, .. }] = value.fragments.as_slice() else {
        return false;
    };
    if bindings.contains_key(&name.to_ascii_lowercase()) {
        return false;
    }
    let [MacroTemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    key.trim().eq_ignore_ascii_case(name)
}

fn is_limit_property(property: &MacroTemplateProperty) -> bool {
    let [MacroTemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    key.trim().eq_ignore_ascii_case("limit")
}

fn is_runtime_branch_key(key: &str) -> bool {
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "if" | "else_if" | "else"
    )
}

fn is_runtime_branch_property(property: &MacroTemplateProperty) -> bool {
    let [MacroTemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    is_runtime_branch_key(key)
}

fn bind_arguments(invocation: &ScriptProperty) -> BTreeMap<String, BoundValue> {
    let mut bindings = BTreeMap::new();
    for argument in &invocation.block {
        let value = argument.scalar.as_ref().map_or_else(
            || BoundValue::Invalid {
                range: argument.block_range.unwrap_or(argument.key_range),
            },
            |(value, range)| BoundValue::Scalar {
                value: value.clone(),
                range: *range,
                quoted_source: argument.quoted_source.clone(),
            },
        );
        bindings.insert(argument.key.to_ascii_lowercase(), value);
    }
    bindings
}
