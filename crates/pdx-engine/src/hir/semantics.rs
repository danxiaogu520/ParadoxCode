//! Rule/profile-aware definitions, references, and localisation semantics.

use pdx_rules::{GameProfile, ProfileDefinitionRule, RuleSet, RuleShape, TypeDescriptor};
use pdx_text::{LogicalPath, TextRange};

use super::{
    HirDefinition, HirLocalisationEntry, HirProperty, HirReference, HirReferenceOrigin, HirScalar,
    ScopeFact, range_within,
};

/// Selects the semantic root context using only immutable rule/profile inputs.
#[must_use]
pub fn semantic_root_context(
    rules: &RuleSet,
    logical_path: Option<&LogicalPath>,
    key: &str,
) -> Option<String> {
    let semantic = &rules.model().semantic;
    if rules.semantic_rules_for_context(key).next().is_some() {
        return Some(key.to_owned());
    }
    let root = format!("root:{key}");
    if rules.semantic_rules_for_context(&root).next().is_some() {
        return Some(root);
    }
    semantic
        .type_root_keys
        .iter()
        .find(|(type_name, roots)| {
            let descriptor = semantic.type_descriptors.get(*type_name);
            (roots.iter().any(|root| root.eq_ignore_ascii_case(key))
                || descriptor.is_some_and(|descriptor| {
                    descriptor.starts_with.as_deref().is_some_and(|prefix| {
                        key.to_ascii_lowercase()
                            .starts_with(&prefix.to_ascii_lowercase())
                    }) || descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|root| {
                            root.eq_ignore_ascii_case("any") || root.eq_ignore_ascii_case(key)
                        })
                    })
                }))
                && semantic.rules.iter().any(|rule| {
                    rule.context
                        .eq_ignore_ascii_case(&format!("root:{type_name}"))
                })
                && descriptor
                    .is_none_or(|descriptor| semantic_type_path_matches(descriptor, logical_path))
        })
        .map(|(type_name, _)| format!("type:{type_name}"))
        .or_else(|| {
            if !logical_path.is_some_and(|path| path.as_str().contains('/')) {
                return None;
            }
            semantic
                .type_descriptors
                .iter()
                .find(|(type_name, descriptor)| {
                    let starts_with = descriptor.starts_with.as_deref().is_some_and(|prefix| {
                        key.to_ascii_lowercase()
                            .starts_with(&prefix.to_ascii_lowercase())
                    });
                    (starts_with
                        || (!semantic.type_root_keys.contains_key(*type_name)
                            && descriptor.skip_root_paths.iter().any(|path| {
                                path.first().is_some_and(|root| {
                                    root.eq_ignore_ascii_case("any")
                                        || root.eq_ignore_ascii_case(key)
                                })
                            })))
                        && rules
                            .semantic_rules_for_context(&format!("root:{type_name}"))
                            .next()
                            .is_some()
                        && semantic_type_path_matches(descriptor, logical_path)
                })
                .map(|(type_name, _)| format!("type:{type_name}"))
                .or_else(|| {
                    semantic
                        .type_descriptors
                        .iter()
                        .find(|(type_name, descriptor)| {
                            !semantic.type_root_keys.contains_key(*type_name)
                                && semantic_type_path_matches(descriptor, logical_path)
                                && rules
                                    .semantic_rules_for_context(&format!("root:{type_name}"))
                                    .next()
                                    .is_some()
                        })
                        .map(|(type_name, _)| format!("type:{type_name}"))
                })
        })
}

fn semantic_type_path_matches(
    descriptor: &TypeDescriptor,
    logical_path: Option<&LogicalPath>,
) -> bool {
    let Some(logical_path) = logical_path else {
        return true;
    };
    let path = logical_path
        .as_str()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if !path.contains('/') {
        return true;
    }
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix = prefix
            .trim_matches('/')
            .strip_prefix("game/")
            .unwrap_or(prefix.trim_matches('/'))
            .to_ascii_lowercase();
        let prefix_match = path == prefix || path.starts_with(&format!("{prefix}/"));
        if !prefix_match {
            return false;
        }
        if descriptor.path_strict
            && path
                .strip_prefix(&format!("{prefix}/"))
                .is_some_and(|rest| rest.contains('/'))
        {
            return false;
        }
    }
    if let Some(file) = descriptor.path_file.as_deref()
        && !path.ends_with(&file.to_ascii_lowercase())
    {
        return false;
    }
    if let Some(extension) = descriptor.path_extension.as_deref() {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        if !path.ends_with(&format!(".{extension}")) {
            return false;
        }
    }
    true
}

pub(super) fn lower_semantics(
    properties: &[HirProperty],
    localisation_entries: &[HirLocalisationEntry],
    bare_values: &[HirScalar],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
    scope_facts: &[ScopeFact],
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
                for rule in profile
                    .conditional_definitions
                    .iter()
                    .filter(|rule| rule.path.matches(path))
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

    let enum_localisation_rules = rules
        .model()
        .semantic
        .rules
        .iter()
        .filter(|rule| {
            matches!(rule.key, pdx_rules::KeyMatcher::Enum(_))
                && matches!(rule.value, pdx_rules::ValueMatcher::Localisation)
        })
        .collect::<Vec<_>>();
    for property in properties {
        if let Some(reference) = semantic_localisation_reference(
            property,
            scope_facts,
            logical_path,
            rules,
            &enum_localisation_rules,
        ) {
            references.push(reference);
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

fn semantic_localisation_reference(
    property: &HirProperty,
    scope_facts: &[ScopeFact],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    enum_localisation_rules: &[&pdx_rules::SemanticRule],
) -> Option<HirReference> {
    let scalar = property.scalar.as_ref()?;
    let fact = scope_facts
        .iter()
        .find(|fact| fact.range == property.key_range);
    let root_context = semantic_root_context(
        rules,
        logical_path,
        property
            .path
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
    );
    let property_parent_path = property
        .path
        .get(1..property.path.len().saturating_sub(1))
        .unwrap_or_default();
    let actual_parent_path = fact
        .as_ref()
        .map_or(property_parent_path, |fact| fact.parent_path.as_slice());
    let context = fact.as_ref().map(|fact| fact.context.as_str());
    let matches_rule = |rule: &pdx_rules::SemanticRule| {
        (context.is_some_and(|context| rule.context.eq_ignore_ascii_case(context))
            || root_context.as_deref().is_some_and(|context| {
                rule.context.eq_ignore_ascii_case(context)
                    || context.strip_prefix("type:").is_some_and(|type_name| {
                        rule.context
                            .eq_ignore_ascii_case(&format!("root:{type_name}"))
                    })
            }))
            && localisation_parent_path_matches(rules, &rule.parent_path, actual_parent_path)
            && matches!(rule.shape, RuleShape::Leaf | RuleShape::LeafValue)
            && matches!(rule.value, pdx_rules::ValueMatcher::Localisation)
            && semantic_localisation_key_matches(rules, &rule.key, &property.key)
    };
    let matches = rules.exact_semantic_rules(&property.key).any(matches_rule)
        || enum_localisation_rules.iter().copied().any(matches_rule);
    matches.then_some(HirReference {
        kind: "localisation".to_owned(),
        name: scalar.value.clone(),
        range: scalar.range,
        origin: HirReferenceOrigin::Semantic,
    })
}

fn localisation_parent_path_matches(
    rules: &RuleSet,
    expected: &[String],
    actual: &[String],
) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if expected.starts_with('<') && expected.ends_with('>') {
                !actual.is_empty()
            } else if let Some(enum_name) = expected
                .strip_prefix("enum[")
                .and_then(|value| value.strip_suffix(']'))
            {
                rules
                    .model()
                    .semantic
                    .enum_values
                    .get(enum_name)
                    .is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| value.eq_ignore_ascii_case(actual))
                    })
            } else {
                expected.eq_ignore_ascii_case(actual)
            }
        })
}

fn semantic_localisation_key_matches(
    rules: &RuleSet,
    matcher: &pdx_rules::KeyMatcher,
    key: &str,
) -> bool {
    match matcher {
        pdx_rules::KeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
        pdx_rules::KeyMatcher::Enum(name) => rules
            .model()
            .semantic
            .enum_values
            .get(name)
            .is_some_and(|values| values.iter().any(|value| value.eq_ignore_ascii_case(key))),
        _ => false,
    }
}

pub(super) fn derived_localisation_references(
    properties: &[HirProperty],
    root_range: TextRange,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
) -> Vec<HirReference> {
    if !logical_path.is_some_and(|path| path.as_str().contains('/')) {
        return Vec::new();
    }
    let mut references = Vec::new();
    for descriptor in rules.model().semantic.type_descriptors.values() {
        if !semantic_type_path_matches(descriptor, logical_path) {
            continue;
        }
        let instances =
            localisation_type_instances(properties, root_range, logical_path, descriptor);
        for (name, range, instance_range) in instances {
            if name.contains('.') {
                continue;
            }
            for binding in rules
                .model()
                .semantic
                .localisation_bindings
                .iter()
                .filter(|binding| binding.type_name.eq_ignore_ascii_case(&descriptor.name))
                .filter(|binding| binding.required)
                .filter(|binding| {
                    localisation_subtype_applies(
                        properties,
                        instance_range,
                        &name,
                        binding.subtype.as_deref(),
                        binding.condition.as_ref(),
                    )
                })
            {
                let Some(template) = binding.template.as_deref() else {
                    continue;
                };
                references.push(HirReference {
                    kind: "localisation".to_owned(),
                    name: template.replace('$', &name),
                    range,
                    origin: HirReferenceOrigin::DerivedLocalisation,
                });
            }
        }
    }
    references
}

fn localisation_type_instances(
    properties: &[HirProperty],
    root_range: TextRange,
    logical_path: Option<&LogicalPath>,
    descriptor: &TypeDescriptor,
) -> Vec<(String, TextRange, TextRange)> {
    if descriptor.type_per_file {
        let Some(path) = logical_path else {
            return Vec::new();
        };
        let Some(file_name) = path.as_str().rsplit('/').next() else {
            return Vec::new();
        };
        let name = file_name
            .rsplit_once('.')
            .map_or(file_name, |(stem, _)| stem);
        return (!name.is_empty())
            .then_some((name.to_owned(), root_range, root_range))
            .into_iter()
            .collect();
    }

    let candidates = if descriptor.skip_root_paths.is_empty() {
        properties
            .iter()
            .filter(|property| property.top_level)
            .collect::<Vec<_>>()
    } else {
        descriptor
            .skip_root_paths
            .iter()
            .flat_map(|skip_path| {
                properties.iter().filter(move |property| {
                    property.path.len() == skip_path.len().saturating_add(1)
                        && property
                            .path
                            .iter()
                            .zip(skip_path)
                            .all(|(actual, expected)| {
                                expected.eq_ignore_ascii_case("any")
                                    || actual.eq_ignore_ascii_case(expected)
                            })
                })
            })
            .collect::<Vec<_>>()
    };

    let mut instances = Vec::<(String, TextRange, TextRange)>::new();
    for property in candidates {
        if !property_key_matches_type(descriptor, &property.key) {
            continue;
        }
        let (name, reference_range) = descriptor
            .name_field
            .as_deref()
            .and_then(|field| nested_property(properties, property, field))
            .and_then(|nested| {
                nested
                    .scalar
                    .as_ref()
                    .map(|scalar| (scalar.value.clone(), scalar.range))
            })
            .unwrap_or_else(|| (property.key.clone(), property.key_range));
        if name.is_empty() {
            continue;
        }
        if !instances.iter().any(|(existing, existing_range, _)| {
            existing.eq_ignore_ascii_case(&name) && *existing_range == reference_range
        }) {
            instances.push((name, reference_range, property.range));
        }
    }
    instances
}

fn property_key_matches_type(descriptor: &TypeDescriptor, key: &str) -> bool {
    descriptor
        .type_key_filter
        .as_ref()
        .is_none_or(|(values, negate)| {
            (values.iter().any(|value| value.eq_ignore_ascii_case(key))) != *negate
        })
}

fn localisation_subtype_applies(
    properties: &[HirProperty],
    instance_range: TextRange,
    instance_name: &str,
    subtype: Option<&str>,
    condition: Option<&pdx_rules::LocalisationBindingCondition>,
) -> bool {
    if let Some(condition) = condition {
        if let Some(prefix) = condition.key_prefix.as_deref() {
            return instance_name
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase());
        }
        let Some(field) = condition.field.as_deref() else {
            return false;
        };
        let Some(instance) = properties
            .iter()
            .find(|property| property.range == instance_range)
        else {
            return false;
        };
        let direct = properties.iter().find(|property| {
            property.path.len() == instance.path.len().saturating_add(1)
                && property
                    .path
                    .iter()
                    .zip(&instance.path)
                    .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
                && range_within(property.range, instance_range)
                && property.key.eq_ignore_ascii_case(field)
        });
        return condition.value.as_deref().is_none_or(|expected| {
            direct
                .and_then(|property| property.scalar.as_ref())
                .is_some_and(|scalar| scalar.value.eq_ignore_ascii_case(expected))
        }) && direct.is_some();
    }
    let Some(subtype) = subtype else {
        return true;
    };
    let (negated, key) = subtype.strip_prefix('!').map_or_else(
        || {
            (
                subtype.starts_with("not_"),
                subtype.strip_prefix("not_").unwrap_or(subtype),
            )
        },
        |key| (true, key),
    );
    let present = properties
        .iter()
        .find(|property| property.range == instance_range)
        .is_some_and(|instance| {
            properties.iter().any(|property| {
                property.path.len() == instance.path.len().saturating_add(1)
                    && property
                        .path
                        .iter()
                        .zip(&instance.path)
                        .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
                    && range_within(property.range, instance_range)
                    && property.key.eq_ignore_ascii_case(key)
            })
        });
    if negated { !present } else { present }
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
