//! Local parameter definitions and references.

use pdx_parser::ParsedFile;
use pdx_rules::GameProfile;
use pdx_text::{LogicalPath, TextRange};

use super::{
    HirParameterConditional, HirParameterDefinition, HirParameterReference,
    HirParameterReferenceKind, HirProperty, range_within,
};

pub(super) fn lower_parameters(
    syntax: &ParsedFile,
    properties: &[HirProperty],
    conditionals: &[HirParameterConditional],
    logical_path: Option<&LogicalPath>,
    profile: Option<&GameProfile>,
) -> (Vec<HirParameterDefinition>, Vec<HirParameterReference>) {
    let (Some(logical_path), Some(profile)) = (logical_path, profile) else {
        return (Vec::new(), Vec::new());
    };
    let rules = profile
        .token_definitions
        .iter()
        .filter(|rule| rule.path.matches(logical_path.as_str()))
        .collect::<Vec<_>>();
    if rules.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut definitions = Vec::new();
    let mut references = Vec::new();
    for rule in &rules {
        for token in syntax
            .tokens()
            .iter()
            .filter(|token| token.kind() == pdx_parser::TokenKind::Bare)
        {
            let Some(raw) = syntax.text(token.range()) else {
                continue;
            };
            for (name, range, name_range) in
                delimited_parameters(raw, token.range(), rule.delimiter)
            {
                let Some(owner_range) = owning_top_level_range(properties, range) else {
                    continue;
                };
                references.push(HirParameterReference {
                    name: name.clone(),
                    range,
                    name_range,
                    owner_range,
                    kind: HirParameterReferenceKind::Substitution,
                });
                infer_parameter_definition(
                    &mut definitions,
                    name,
                    range,
                    name_range,
                    owner_range,
                    rule.delimiter,
                );
            }
        }
    }
    for conditional in conditionals {
        let Some(owner_range) = owning_top_level_range(properties, conditional.range) else {
            continue;
        };
        references.push(HirParameterReference {
            name: conditional.name.clone(),
            range: conditional.condition_range,
            name_range: conditional.name_range,
            owner_range,
            kind: HirParameterReferenceKind::Conditional,
        });
        infer_parameter_definition(
            &mut definitions,
            conditional.name.clone(),
            conditional.condition_range,
            conditional.name_range,
            owner_range,
            rules[0].delimiter,
        );
    }
    definitions.sort_by_key(|definition| definition.range.start());
    references.sort_by_key(|reference| reference.range.start());
    (definitions, references)
}

fn owning_top_level_range(properties: &[HirProperty], occurrence: TextRange) -> Option<TextRange> {
    properties
        .iter()
        .filter(|property| property.top_level && range_within(occurrence, property.range))
        .map(|property| property.range)
        .next()
}

fn infer_parameter_definition(
    definitions: &mut Vec<HirParameterDefinition>,
    name: String,
    range: TextRange,
    name_range: TextRange,
    owner_range: TextRange,
    delimiter: char,
) {
    if definitions.iter().any(|definition| {
        definition.owner_range == owner_range
            && definition.delimiter == delimiter
            && definition.name.eq_ignore_ascii_case(&name)
    }) {
        return;
    }
    definitions.push(HirParameterDefinition {
        name,
        range,
        name_range,
        owner_range,
        delimiter,
    });
}

fn delimited_parameters(
    raw: &str,
    token_range: TextRange,
    delimiter: char,
) -> Vec<(String, TextRange, TextRange)> {
    let mut parameters = Vec::new();
    let mut opening: Option<usize> = None;
    for (offset, character) in raw.char_indices() {
        if character != delimiter {
            continue;
        }
        if let Some(start) = opening.take() {
            let delimiter_len = delimiter.len_utf8();
            if start + delimiter_len >= offset {
                continue;
            }
            let name_start = start.saturating_add(delimiter_len);
            let token_start = usize::try_from(token_range.start()).unwrap_or(0);
            let absolute = |relative: usize| {
                u32::try_from(token_start.saturating_add(relative)).unwrap_or(u32::MAX)
            };
            let range = TextRange::new(absolute(start), absolute(offset + delimiter_len))
                .unwrap_or(token_range);
            let name_range =
                TextRange::new(absolute(name_start), absolute(offset)).unwrap_or(token_range);
            parameters.push((raw[name_start..offset].to_owned(), range, name_range));
        } else {
            opening = Some(offset);
        }
    }
    parameters
}
