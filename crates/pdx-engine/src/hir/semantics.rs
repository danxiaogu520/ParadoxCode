//! Rule/profile-aware definitions, references, and localisation semantics.

use pdx_parser::parse_quoted_script;
use pdx_rules::{
    GameProfile, KeyMatcher, ProfileDefinitionRule, RuleSet, RuleShape, TypeDescriptor,
};
use pdx_text::{LogicalPath, TextRange, TextSize};

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
    semantic_root_context_with_confidence(rules, logical_path, key).0
}

/// Returns whether the selected context came from the weakest path-only fallback.
///
/// A context chosen by the plain path fallback is a guess from the directory layout
/// rather than a key/filter-declared match, so diagnostics derived from it are less
/// trustworthy and callers may downgrade their severity.
#[must_use]
pub fn semantic_root_context_is_fallback(
    rules: &RuleSet,
    logical_path: Option<&LogicalPath>,
    key: &str,
) -> bool {
    semantic_root_context_with_confidence(rules, logical_path, key).1
}

fn semantic_root_context_with_confidence(
    rules: &RuleSet,
    logical_path: Option<&LogicalPath>,
    key: &str,
) -> (Option<String>, bool) {
    if let Some(context) = scripted_macro_path_context(rules, logical_path) {
        return (Some(context), false);
    }
    let semantic = &rules.model().semantic;
    // A top-level key may name a rule context directly (`trigger`, `effect`). A
    // `root:<key>` context that belongs to a type descriptor must not be selected in
    // an unrelated directory (for example `fervor` inside common/static_modifiers).
    let key_context_matches_path = |candidate: &str| {
        semantic
            .type_descriptors
            .get(candidate)
            .is_none_or(|descriptor| semantic_type_path_matches(descriptor, logical_path))
    };
    if rules.semantic_rules_for_context(key).next().is_some() {
        return (Some(key.to_owned()), false);
    }
    let root = format!("root:{key}");
    if rules.semantic_rules_for_context(&root).next().is_some() && key_context_matches_path(key) {
        return (Some(root), false);
    }
    if let Some(context) = semantic
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
        })
        .or_else(|| {
            semantic
                .type_descriptors
                .iter()
                .find(|(type_name, descriptor)| {
                    !semantic.type_root_keys.contains_key(*type_name)
                        && semantic_type_path_matches(descriptor, logical_path)
                        && descriptor
                            .type_key_filter
                            .as_ref()
                            .is_some_and(|(values, negate)| {
                                values.iter().any(|value| value.eq_ignore_ascii_case(key))
                                    != *negate
                            })
                        && rules
                            .semantic_rules_for_context(&format!("root:{type_name}"))
                            .next()
                            .is_some()
                })
                .map(|(type_name, _)| format!("type:{type_name}"))
        })
    {
        return (Some(context), false);
    }
    // Weakest match: any descriptor whose directory contains the file and whose type
    // has rules, without any key/filter constraint.
    let context = semantic
        .type_descriptors
        .iter()
        .find(|(type_name, descriptor)| {
            !semantic.type_root_keys.contains_key(*type_name)
                && descriptor.type_key_filter.is_none()
                && semantic_type_path_matches(descriptor, logical_path)
                && rules
                    .semantic_rules_for_context(&format!("root:{type_name}"))
                    .next()
                    .is_some()
        })
        .map(|(type_name, _)| format!("type:{type_name}"));
    (context.clone(), context.is_some())
}

/// Selects the semantic context for a type whose definition is the complete file.
///
/// Unlike [`semantic_root_context`], this lookup is intentionally independent of a top-level
/// property key. A `type_per_file` descriptor describes the document root itself, so selecting
/// its context from an arbitrary first property can otherwise make validation depend on field
/// order or validate every field as a separate root container.
#[must_use]
pub fn semantic_file_root_context(
    rules: &RuleSet,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    let logical_path = logical_path?;
    if !logical_path.as_str().contains('/') {
        return None;
    }
    rules
        .model()
        .semantic
        .type_descriptors
        .iter()
        .find(|(type_name, descriptor)| {
            descriptor.type_per_file
                && semantic_type_path_matches(descriptor, Some(logical_path))
                && rules
                    .semantic_rules_for_context(&format!("root:{type_name}"))
                    .next()
                    .is_some()
        })
        .map(|(type_name, _)| format!("type:{type_name}"))
}

pub(super) fn scripted_macro_path_context(
    rules: &RuleSet,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    let logical_path = logical_path?;
    if !logical_path.as_str().contains('/') {
        return None;
    }
    rules
        .model()
        .semantic
        .type_descriptors
        .values()
        .filter_map(|descriptor| {
            let macro_descriptor = descriptor.scripted_macro.as_ref()?;
            if !macro_descriptor.macro_enabled
                || !semantic_type_path_matches(descriptor, Some(logical_path))
            {
                return None;
            }
            let context = macro_descriptor.body_context.trim();
            (!context.is_empty()).then(|| context.to_owned())
        })
        .next()
}

pub(super) fn scripted_macro_type_context(rules: &RuleSet, type_name: &str) -> Option<String> {
    rules
        .model()
        .semantic
        .type_descriptors
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(type_name))
        .and_then(|(_, descriptor)| descriptor.scripted_macro.as_ref())
        .filter(|descriptor| descriptor.macro_enabled)
        .map(|descriptor| descriptor.body_context.trim().to_owned())
        .filter(|context| !context.is_empty())
}

pub(super) fn is_scripted_macro_type(rules: &RuleSet, type_name: &str) -> bool {
    scripted_macro_type_context(rules, type_name).is_some()
}

/// Whether a top-level property key may be a type instance for `descriptor`.
///
/// Descriptors with an enumerated root-key set (`type_root_keys`) reject unrelated file
/// headers; without an enumeration every root key is a candidate instance.
#[must_use]
pub(super) fn semantic_type_root_key_allowed(
    rules: &RuleSet,
    descriptor: &TypeDescriptor,
    key: &str,
) -> bool {
    let Some(roots) = rules.model().semantic.type_root_keys.get(&descriptor.name) else {
        return true;
    };
    roots.iter().any(|root| root.eq_ignore_ascii_case(key))
}

/// Returns whether a descriptor's path/file/extension selectors match a logical path.
#[must_use]
pub fn semantic_type_path_matches(
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
    let (_directory, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    if !path.contains('/') {
        return true;
    }
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix = prefix
            .trim_matches('/')
            .strip_prefix("game/")
            .unwrap_or(prefix.trim_matches('/'))
            .to_ascii_lowercase();
        // The file may sit directly under the descriptor directory, or under any
        // directory that ends with it (an absolute path or a cache layout whose
        // root prefix differs from the game-relative one).
        let prefix_match = path == prefix
            || path.starts_with(&format!("{prefix}/"))
            || _directory == prefix
            || _directory.ends_with(&format!("/{prefix}"));
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
        && !file_name.eq_ignore_ascii_case(file)
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

/// Indexed view over one file's scope facts.
///
/// Facts are produced in document order; every consumer previously performed a
/// linear `find`/`rfind` per property, which made lowering quadratic on large
/// files. The exact lookup is a hash hit, and the nearest-preceding lookup
/// binary-searches the document order and then walks back only across the
/// property's preceding siblings.
pub(super) struct ScopeFactIndex<'a> {
    facts: &'a [ScopeFact],
    exact: std::collections::HashMap<TextRange, usize>,
}

impl<'a> ScopeFactIndex<'a> {
    pub(super) fn new(facts: &'a [ScopeFact]) -> Self {
        let mut exact = std::collections::HashMap::with_capacity(facts.len());
        for (index, fact) in facts.iter().enumerate() {
            // `Iterator::find` semantics keep the first fact for a range.
            exact.entry(fact.range).or_insert(index);
        }
        Self { facts, exact }
    }

    /// The fact recorded for exactly this key range, if any.
    pub(super) fn exact(&self, key_range: TextRange) -> Option<&'a ScopeFact> {
        self.exact.get(&key_range).map(|&index| &self.facts[index])
    }

    /// The nearest fact that ends at or before `offset`.
    pub(super) fn preceding(&self, offset: TextSize) -> Option<&'a ScopeFact> {
        // Facts whose start is at/after the offset cannot end before it.
        let upper = self
            .facts
            .partition_point(|fact| fact.range.start() < offset);
        self.facts[..upper]
            .iter()
            .rev()
            .find(|fact| fact.range.end() <= offset)
    }
}

/// Per-file cache of root-context lookups keyed by root key.
///
/// Most files have a handful of distinct root keys, and the lookup consults
/// several rule indexes with case-normalized allocations on every call.
pub(super) struct RootContextCache {
    logical_path: Option<LogicalPath>,
    entries: std::collections::HashMap<String, Option<String>>,
}

impl RootContextCache {
    pub(super) fn new(logical_path: Option<&LogicalPath>) -> Self {
        Self {
            logical_path: logical_path.cloned(),
            entries: std::collections::HashMap::new(),
        }
    }

    pub(super) fn get(&mut self, rules: &RuleSet, root_key: &str) -> Option<String> {
        self.entries
            .entry(root_key.to_owned())
            .or_insert_with(|| semantic_root_context(rules, self.logical_path.as_ref(), root_key))
            .clone()
    }
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
    let fact_index = ScopeFactIndex::new(scope_facts);
    let mut root_contexts = RootContextCache::new(logical_path);
    let property_children = super::scope::property_children(properties);

    if let Some(profile) = profile {
        for (property_index, property) in properties.iter().enumerate() {
            if property.top_level {
                if let Some(rule) = profile
                    .definition(path, &property.key)
                    .filter(|rule| !rule.requires_value || property.value_range.is_some())
                {
                    definitions.push(definition_from_rule(properties, property_index, rule));
                }
                for rule in profile
                    .conditional_definitions
                    .iter()
                    .filter(|rule| rule.path.matches(path))
                {
                    if nested_property(properties, property_index, &rule.required_field)
                        .and_then(|nested| nested.scalar.as_ref())
                        .is_some_and(|scalar| {
                            scalar.value.eq_ignore_ascii_case(&rule.required_value)
                        })
                        && nested_property(properties, property_index, &rule.absent_field).is_none()
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
                    // Direct children are precomputed once per file in document
                    // order; the previous per-property scan was quadratic.
                    for &child_index in &property_children[property_index] {
                        let child = &properties[child_index];
                        definitions.push(HirDefinition {
                            kind: rule.kind.clone(),
                            name: child.key.clone(),
                            range: child.range,
                            selection_range: child.key_range,
                        });
                    }
                }
            }
            if let Some(reference) = reference_from_property(profile, property, logical_path)
                && !semantic_rules_describe_property(
                    property,
                    &fact_index,
                    &mut root_contexts,
                    rules,
                )
            {
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

        for (property_index, property) in properties.iter().enumerate() {
            let Some(rule) = profile.container_value_definition(&property.key) else {
                continue;
            };
            let Some(named) = nested_property(properties, property_index, &rule.name_field) else {
                continue;
            };
            let Some(scalar) = named.scalar.as_ref() else {
                continue;
            };
            if !scalar.value.is_empty() {
                definitions.push(HirDefinition {
                    kind: rule.kind.clone(),
                    name: scalar.value.clone(),
                    range: named.range,
                    selection_range: scalar.range,
                });
            }
        }

        definitions.extend(quoted_script_value_definitions(properties, profile));
        definitions.extend(scripted_localisation_definitions(
            properties,
            logical_path,
            profile,
        ));
    }

    definitions.extend(semantic_type_definitions(properties, logical_path, rules));
    deduplicate_definitions(&mut definitions);

    references.extend(scripted_macro_references(properties, &fact_index, rules));
    references.extend(semantic_typed_references(
        properties,
        &fact_index,
        rules,
        profile,
    ));

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
            &fact_index,
            &mut root_contexts,
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

fn semantic_rules_describe_property(
    property: &HirProperty,
    fact_index: &ScopeFactIndex<'_>,
    root_contexts: &mut RootContextCache,
    rules: &RuleSet,
) -> bool {
    let fact = fact_index.exact(property.key_range);
    let root_context = root_contexts.get(
        rules,
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
    // Dynamic scope-link blocks (country tags, `event_target:name`, province ids) are
    // not statically selectable, so they emit no fact of their own. The nearest
    // preceding fact still names the semantic container for their children.
    let context = fact.map(|fact| fact.context.as_str()).or_else(|| {
        fact_index
            .preceding(property.key_range.start())
            .map(|fact| fact.context.as_str())
    });
    rules.exact_semantic_rules(&property.key).any(|rule| {
        (context.is_some_and(|context| rule.context.eq_ignore_ascii_case(context))
            || root_context.as_deref().is_some_and(|context| {
                rule.context.eq_ignore_ascii_case(context)
                    || context.strip_prefix("type:").is_some_and(|type_name| {
                        rule.context
                            .eq_ignore_ascii_case(&format!("root:{type_name}"))
                    })
            }))
            && localisation_parent_path_matches(rules, &rule.parent_path, actual_parent_path)
    })
}

fn quoted_script_value_definitions(
    properties: &[HirProperty],
    profile: &GameProfile,
) -> Vec<HirDefinition> {
    let mut definitions = Vec::new();
    for property in properties {
        let Some(scalar) = property.scalar.as_ref() else {
            continue;
        };
        if !scalar.quoted || !profile.indexes_quoted_script_definitions(&property.key) {
            continue;
        }
        let source = format!("\"{}\"", scalar.value);
        let Some(script) = parse_quoted_script(&source) else {
            continue;
        };
        let collected = super::collector::collect(script.parsed());
        for embedded in &collected.properties {
            let parent_key = embedded.path.iter().rev().nth(1).map(String::as_str);
            let Some(kind) = profile.value_definition_kind(&embedded.key, parent_key) else {
                continue;
            };
            let Some(value) = embedded
                .scalar
                .as_ref()
                .filter(|value| !value.value.is_empty())
            else {
                continue;
            };
            let map_range = |range: TextRange| {
                let relative = script.source_map().decoded_range(range)?;
                TextRange::new(
                    scalar.range.start().checked_add(relative.start())?,
                    scalar.range.start().checked_add(relative.end())?,
                )
            };
            let Some(range) = map_range(embedded.range) else {
                continue;
            };
            let Some(selection_range) = map_range(value.range) else {
                continue;
            };
            definitions.push(HirDefinition {
                kind: kind.to_owned(),
                name: value.value.clone(),
                range,
                selection_range,
            });
        }
    }
    definitions
}

fn semantic_type_definitions(
    properties: &[HirProperty],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
) -> Vec<HirDefinition> {
    let mut definitions = Vec::new();
    let Some(logical_path) = logical_path else {
        return definitions;
    };
    if !logical_path.as_str().contains('/') {
        return definitions;
    }
    for descriptor in rules.model().semantic.type_descriptors.values() {
        if !semantic_type_path_matches(descriptor, Some(logical_path)) {
            continue;
        }
        if descriptor.type_per_file {
            let path = logical_path;
            let Some(file_name) = path.as_str().rsplit('/').next() else {
                continue;
            };
            let name = file_name
                .rsplit_once('.')
                .map_or(file_name, |(stem, _)| stem);
            if !name.is_empty() {
                definitions.push(HirDefinition {
                    kind: descriptor.name.clone(),
                    name: name.to_owned(),
                    range: properties
                        .iter()
                        .find(|property| property.top_level)
                        .map_or(TextRange::empty(0), |property| property.range),
                    selection_range: properties
                        .iter()
                        .find(|property| property.top_level)
                        .map_or(TextRange::empty(0), |property| property.key_range),
                });
            }
            continue;
        }
        if descriptor.skip_root_paths.is_empty() {
            // A descriptor whose legal root keys are enumerated (`type_root_keys`) must not
            // collect unrelated file headers as type instances. For example EU4 event files
            // accept `namespace` and `normal_or_historical_nations` at the root, but only
            // `country_event`/`province_event` are event instances.
            for property in properties.iter().filter(|property| property.top_level) {
                if !semantic_type_root_key_allowed(rules, descriptor, &property.key) {
                    continue;
                }
                push_type_definition(&mut definitions, properties, descriptor, property);
            }
        } else {
            for root in properties.iter().filter(|property| property.top_level) {
                for path in &descriptor.skip_root_paths {
                    collect_type_path_definitions(
                        &mut definitions,
                        properties,
                        descriptor,
                        root,
                        path,
                    );
                }
            }
        }
    }
    definitions
}

/// Collects path-driven scripted-localisation definitions.
///
/// CWTools reads `name = ...` from every top-level clause under a profile-declared scripted
/// localisation directory.  It intentionally does not require a particular clause key because
/// game versions use both `defined_text` and custom wrapper spellings.  Keeping this extraction
/// in profile-aware HIR lowering makes disk files and overlays produce the same definition facts,
/// while the profile owns the concrete directory spellings.
fn scripted_localisation_definitions(
    properties: &[HirProperty],
    logical_path: Option<&LogicalPath>,
    profile: &GameProfile,
) -> Vec<HirDefinition> {
    let Some(logical_path) = logical_path else {
        return Vec::new();
    };
    if !profile.is_scripted_localisation_path(logical_path.as_str()) {
        return Vec::new();
    }
    let mut definitions = Vec::new();
    for property in properties.iter().filter(|property| property.top_level) {
        for child in immediate_children(properties, property)
            .filter(|child| child.key.eq_ignore_ascii_case("name"))
        {
            let Some(scalar) = child.scalar.as_ref() else {
                continue;
            };
            if scalar.value.is_empty() || scalar.value.contains('$') {
                continue;
            }
            definitions.push(HirDefinition {
                kind: "defined_text".to_owned(),
                name: scalar.value.clone(),
                range: property.range,
                selection_range: scalar.range,
            });
        }
    }
    definitions
}

fn collect_type_path_definitions(
    definitions: &mut Vec<HirDefinition>,
    properties: &[HirProperty],
    descriptor: &TypeDescriptor,
    property: &HirProperty,
    path: &[String],
) {
    let Some(head) = path.first() else {
        for child in immediate_children(properties, property) {
            push_type_definition(definitions, properties, descriptor, child);
        }
        return;
    };
    if !head.eq_ignore_ascii_case("any") && !head.eq_ignore_ascii_case(&property.key) {
        return;
    }
    if path.len() == 1 {
        for child in immediate_children(properties, property) {
            push_type_definition(definitions, properties, descriptor, child);
        }
    } else {
        for child in immediate_children(properties, property) {
            collect_type_path_definitions(definitions, properties, descriptor, child, &path[1..]);
        }
    }
}

fn immediate_children<'property>(
    properties: &'property [HirProperty],
    parent: &HirProperty,
) -> impl Iterator<Item = &'property HirProperty> {
    properties.iter().filter(|candidate| {
        candidate.path.len() == parent.path.len().saturating_add(1)
            && candidate.path.starts_with(&parent.path)
            && range_within(candidate.range, parent.range)
    })
}

fn push_type_definition(
    definitions: &mut Vec<HirDefinition>,
    properties: &[HirProperty],
    descriptor: &TypeDescriptor,
    property: &HirProperty,
) {
    if !descriptor
        .type_key_filter
        .as_ref()
        .is_none_or(|(values, negate)| {
            values
                .iter()
                .any(|value| value.eq_ignore_ascii_case(&property.key))
                != *negate
        })
    {
        return;
    }
    let (name, selection_range) = descriptor
        .name_field
        .as_deref()
        .and_then(|field| {
            immediate_children(properties, property)
                .find(|child| child.key.eq_ignore_ascii_case(field))
        })
        .and_then(|child| {
            child
                .scalar
                .as_ref()
                .map(|scalar| (scalar.value.clone(), scalar.range))
        })
        .unwrap_or_else(|| (property.key.clone(), property.key_range));
    if name.is_empty() || name.contains('$') {
        return;
    }
    definitions.push(HirDefinition {
        kind: descriptor.name.clone(),
        name,
        range: property.range,
        selection_range,
    });
}

fn deduplicate_definitions(definitions: &mut Vec<HirDefinition>) {
    let mut seen = std::collections::BTreeSet::new();
    definitions.retain(|definition| {
        seen.insert((
            definition.kind.to_ascii_lowercase(),
            definition.name.to_ascii_lowercase(),
            definition.range,
        ))
    });
}

fn scripted_macro_references(
    properties: &[HirProperty],
    fact_index: &ScopeFactIndex<'_>,
    rules: &RuleSet,
) -> Vec<HirReference> {
    let mut references = Vec::new();
    for property in properties {
        let Some(fact) = fact_index.exact(property.key_range) else {
            continue;
        };
        if property.top_level || !is_concrete_scripted_key(&property.key) {
            continue;
        }

        for rule in rules.semantic_rules_for_context(&fact.context) {
            let type_name = match &rule.key {
                KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
                    if is_scripted_macro_type(rules, type_name) =>
                {
                    type_name
                }
                _ => continue,
            };
            let Some(body_context) = scripted_macro_type_context(rules, type_name) else {
                continue;
            };
            if !body_context.eq_ignore_ascii_case(&fact.context)
                || !semantic_paths_match(&rule.parent_path, &fact.parent_path)
                || !property_matches_scripted_rule(property, rule, rules)
            {
                continue;
            }
            references.push(HirReference {
                kind: type_name.clone(),
                name: property.key.clone(),
                range: property.key_range,
                origin: HirReferenceOrigin::ScriptedMacro,
            });
        }
    }
    references
}

fn is_concrete_scripted_key(key: &str) -> bool {
    !key.is_empty()
        && !key
            .chars()
            .any(|character| matches!(character, '$' | '[' | ']'))
}

fn semantic_paths_match(expected: &[String], actual: &[String]) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            (expected.starts_with('<') && expected.ends_with('>'))
                || expected.eq_ignore_ascii_case(actual)
        })
}

fn property_matches_scripted_rule(
    property: &HirProperty,
    rule: &pdx_rules::SemanticRule,
    rules: &RuleSet,
) -> bool {
    if rule
        .operator
        .as_deref()
        .is_some_and(|operator| property.operator.as_deref() != Some(operator))
    {
        return false;
    }
    match rule.shape {
        RuleShape::Leaf | RuleShape::LeafValue | RuleShape::QuotedScript => {
            let Some(scalar) = property.scalar.as_ref() else {
                return false;
            };
            if matches!(rule.shape, RuleShape::QuotedScript) && !scalar.quoted {
                return false;
            }
            rule.value.matches(
                &scalar.value,
                |_type_name, value| !value.is_empty(),
                |enum_name, value| {
                    rules
                        .model()
                        .semantic
                        .enum_values
                        .get(enum_name)
                        .is_some_and(|values| {
                            values
                                .iter()
                                .any(|candidate| candidate.eq_ignore_ascii_case(value))
                        })
                },
                |_scope, value| !value.is_empty(),
            )
        }
        RuleShape::Node | RuleShape::ValueClause => {
            property.value_range.is_some()
                && matches!(
                    rule.value,
                    pdx_rules::ValueMatcher::AnyScalar | pdx_rules::ValueMatcher::Opaque(_)
                )
        }
    }
}

fn semantic_localisation_reference(
    property: &HirProperty,
    fact_index: &ScopeFactIndex<'_>,
    root_contexts: &mut RootContextCache,
    rules: &RuleSet,
    enum_localisation_rules: &[&pdx_rules::SemanticRule],
) -> Option<HirReference> {
    let scalar = property.scalar.as_ref()?;
    // Quoted values are literal text to the game, not localisation key references. A spacer such
    // as `custom_tooltip = " "` must not produce an unknown-symbol diagnostic.
    if scalar.quoted || scalar.value.contains('$') {
        return None;
    }
    let fact = fact_index.exact(property.key_range);
    let root_context = root_contexts.get(
        rules,
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
    // Dynamic scope-link blocks emit no fact of their own; the nearest preceding
    // fact still names the semantic container for their children.
    let context = fact.map(|fact| fact.context.as_str()).or_else(|| {
        fact_index
            .preceding(property.key_range.start())
            .map(|fact| fact.context.as_str())
    });
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

/// Collects scalar values whose first-party semantic rule points at a workspace symbol type.
///
/// Profile references cover the common shorthand forms (`event = foo.1`), but event effects and
/// many other commands use a block with a typed child (`country_event = { id = foo.1 }`).  The
/// child value is described by the semantic rule database rather than by the profile's flat
/// property-key table.  Lowering it here keeps the fact available to both disk indexes and live
/// overlays, so navigation does not need a game-specific special case.
fn semantic_typed_references(
    properties: &[HirProperty],
    fact_index: &ScopeFactIndex<'_>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> Vec<HirReference> {
    let mut references = Vec::new();
    for property in properties {
        let Some(scalar) = property.scalar.as_ref() else {
            continue;
        };
        if scalar.quoted
            || scalar.value.is_empty()
            || scalar.value.contains('$')
            || scalar.value.eq_ignore_ascii_case("yes")
            || scalar.value.eq_ignore_ascii_case("no")
        {
            continue;
        }
        let Some(fact) = fact_index
            .exact(property.key_range)
            .or_else(|| fact_index.preceding(property.key_range.start()))
        else {
            continue;
        };
        let property_parent_path = property
            .path
            .get(1..property.path.len().saturating_sub(1))
            .unwrap_or_default();
        let actual_parent_path = if fact.range == property.key_range {
            fact.parent_path.as_slice()
        } else {
            property_parent_path
        };

        let mut kinds = Vec::<String>::new();
        let contexts = semantic_reference_contexts(profile, &fact.context);
        for rule in contexts
            .iter()
            .flat_map(|context| rules.semantic_rules_for_context_key(context, &property.key))
            .filter(|rule| {
                semantic_reference_context_matches(profile, &fact.context, &rule.context)
            })
            .filter(|rule| {
                matches!(rule.shape, RuleShape::Leaf)
                    && localisation_parent_path_matches(
                        rules,
                        &rule.parent_path,
                        actual_parent_path,
                    )
                    && semantic_reference_key_matches(rules, &rule.key, &property.key)
                    && rule
                        .operator
                        .as_deref()
                        .is_none_or(|operator| property.operator.as_deref() == Some(operator))
            })
        {
            let pdx_rules::ValueMatcher::Type(type_name) = &rule.value else {
                continue;
            };
            let base = type_name
                .split_once('.')
                .map_or(type_name.as_str(), |(base, _)| base);
            let kind = profile
                .and_then(|profile| {
                    profile
                        .member_kind_alias(type_name)
                        .or_else(|| profile.member_kind_alias(base))
                })
                .unwrap_or(base)
                .to_owned();
            if !kinds
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&kind))
            {
                kinds.push(kind);
            }
        }
        // A scalar can have several semantic alternatives (for example a culture key may also
        // accept an enum or a scope).  Emit a typed reference only when all typed alternatives
        // agree on the workspace kind; otherwise navigation must not guess an interpretation.
        if kinds.len() == 1 {
            references.push(HirReference {
                kind: kinds.remove(0),
                name: scalar.value.clone(),
                range: scalar.range,
                origin: HirReferenceOrigin::SemanticTyped,
            });
        }
    }
    references
}

fn semantic_reference_contexts(profile: Option<&GameProfile>, actual: &str) -> Vec<String> {
    let mut contexts = vec![actual.to_owned()];
    if let Some(type_name) = actual.strip_prefix("type:") {
        contexts.push(format!("root:{type_name}"));
    }
    let mut index = 0;
    while let Some(context) = contexts.get(index).cloned() {
        if let Some(profile) = profile {
            for ancestor in profile.inherited_semantic_contexts(&context) {
                if !contexts
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(ancestor))
                {
                    contexts.push(ancestor.clone());
                }
            }
        }
        index += 1;
    }
    contexts
}

fn semantic_reference_context_matches(
    profile: Option<&GameProfile>,
    actual: &str,
    expected: &str,
) -> bool {
    actual.eq_ignore_ascii_case(expected)
        || actual.strip_prefix("type:").is_some_and(|type_name| {
            expected
                .strip_prefix("root:")
                .is_some_and(|root_type| root_type.eq_ignore_ascii_case(type_name))
        })
        || profile.is_some_and(|profile| profile.semantic_context_inherits(actual, expected))
}

fn semantic_reference_key_matches(rules: &RuleSet, matcher: &KeyMatcher, key: &str) -> bool {
    match matcher {
        KeyMatcher::Exact(expected) => expected.eq_ignore_ascii_case(key),
        KeyMatcher::Enum(enum_name) => rules
            .model()
            .semantic
            .enum_values
            .get(enum_name)
            .is_some_and(|values| values.iter().any(|value| value.eq_ignore_ascii_case(key))),
        KeyMatcher::AnyScalar => !key.is_empty(),
        KeyMatcher::Date => matcher.matches(key, |_, _| false, |_, _| false),
        KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => false,
    }
}

fn localisation_parent_path_matches(
    rules: &RuleSet,
    expected: &[String],
    actual: &[String],
) -> bool {
    // Scope-link keys (country tags, `event_target:name`, province ids) extend the
    // structural path without changing the semantic container, so the rule parent
    // path may be a suffix of the observed path.
    if expected.len() > actual.len() {
        return false;
    }
    let expected_offset = actual.len() - expected.len();
    expected
        .iter()
        .zip(actual.iter().skip(expected_offset))
        .all(|(expected, actual)| {
            if expected.starts_with('<') && expected.ends_with('>') {
                !actual.is_empty()
            } else if expected.eq_ignore_ascii_case("date_field") {
                pdx_rules::ValueMatcher::Date.matches(
                    actual,
                    |_, _| false,
                    |_, _| false,
                    |_, _| false,
                )
            } else if expected.eq_ignore_ascii_case("int") {
                actual.parse::<i64>().is_ok()
            } else if expected.eq_ignore_ascii_case("float") {
                actual.parse::<f64>().is_ok()
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
    include_optional: bool,
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
                // Required generated templates participate in diagnostics. Explicit fields are
                // already present in source and validated by their normal localisation rule;
                // hover asks for those associations (and optional generated templates) through
                // the `include_optional` path, but they stay out of diagnostics.
                .filter(|binding| include_optional || binding.required)
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
                if let Some(template) = binding.template.as_deref() {
                    references.push(HirReference {
                        kind: "localisation".to_owned(),
                        name: template.replace('$', &name),
                        range,
                        origin: HirReferenceOrigin::DerivedLocalisation,
                    });
                    continue;
                }
                // An explicit field mapping (for example an event's `title`) does not
                // have a generated key.  It still belongs to the type instance, so retain
                // the field value as a localisation reference at its own source range.  The
                // ordinary semantic/profile lowering already validates this value; this
                // derived entry only associates it with the instance for navigation and hover.
                let Some(field) = binding.explicit_field.as_deref() else {
                    continue;
                };
                let Some(instance_index) = properties
                    .iter()
                    .position(|property| property.range == instance_range)
                else {
                    continue;
                };
                let Some(field_property) = nested_property(properties, instance_index, field)
                else {
                    continue;
                };
                let Some(field_value) = field_property.scalar.as_ref() else {
                    continue;
                };
                if field_value.quoted
                    || field_value.value.is_empty()
                    || field_value.value.contains('$')
                {
                    continue;
                }
                references.push(HirReference {
                    kind: "localisation".to_owned(),
                    name: field_value.value.clone(),
                    range: field_value.range,
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
            .enumerate()
            .filter(|(_, property)| property.top_level)
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    } else {
        descriptor
            .skip_root_paths
            .iter()
            .flat_map(|skip_path| {
                properties.iter().enumerate().filter(move |(_, property)| {
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
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    };

    let mut instances = Vec::<(String, TextRange, TextRange)>::new();
    for property_index in candidates {
        // Only container blocks declare type instances; scalar fields inside a parent
        // block (for example `can_form_personal_unions = yes` inside a religion group)
        // must not be treated as instances of the child type.
        let property = &properties[property_index];
        if property.scalar.is_some() || !property_key_matches_type(descriptor, &property.key) {
            continue;
        }
        let (name, reference_range) = descriptor
            .name_field
            .as_deref()
            .and_then(|field| nested_property(properties, property_index, field))
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
    property_index: usize,
    rule: &ProfileDefinitionRule,
) -> HirDefinition {
    let property = &properties[property_index];
    let named = rule
        .name_field
        .as_deref()
        .and_then(|field| nested_property(properties, property_index, field))
        .and_then(|nested| nested.scalar.as_ref());
    HirDefinition {
        kind: rule.kind.clone(),
        name: named.map_or_else(|| property.key.clone(), |scalar| scalar.value.clone()),
        range: property.range,
        selection_range: named.map_or(property.key_range, |scalar| scalar.range),
    }
}

fn reference_from_property(
    profile: &GameProfile,
    property: &HirProperty,
    logical_path: Option<&LogicalPath>,
) -> Option<HirReference> {
    let rule = profile.reference_rule(&property.key)?;
    let scalar = property.scalar.as_ref()?;
    // Quoted values are literal text to the game (names, resource identifiers, tooltip
    // spacers), not symbol references.
    if scalar.quoted
        || scalar.value.is_empty()
        || scalar.value.eq_ignore_ascii_case("yes")
        || scalar.value.eq_ignore_ascii_case("no")
        || scalar.value.parse::<f64>().is_ok()
    {
        return None;
    }
    if logical_path.is_some_and(|path| {
        rule.excluded_paths
            .iter()
            .any(|matcher| matcher.matches(path.as_str()))
    }) {
        return None;
    }
    Some(HirReference {
        kind: rule.kind.clone(),
        name: scalar.value.clone(),
        range: scalar.range,
        origin: HirReferenceOrigin::Profile,
    })
}

fn nested_property<'hir>(
    properties: &'hir [HirProperty],
    parent_index: usize,
    wanted: &str,
) -> Option<&'hir HirProperty> {
    // Properties are collected in document order, so the parent's descendants
    // form a contiguous window that ends at the first property at or above the
    // parent's depth. Scanning that window (instead of every property in the
    // file) keeps conditional-definition extraction linear overall.
    let parent = &properties[parent_index];
    properties[parent_index + 1..]
        .iter()
        .take_while(|property| {
            property.path.len() > parent.path.len()
                && property.path.starts_with(&parent.path)
                && range_within(property.range, parent.range)
        })
        .find(|property| property.key.eq_ignore_ascii_case(wanted))
}
