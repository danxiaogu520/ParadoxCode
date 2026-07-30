//! Rule-aware, game-independent semantic lowering boundary.

use std::sync::Arc;

use pdx_rules::{
    GameProfile, KeyMatcher, ProfileDefinitionRule, RuleSet, RuleShape, SemanticRule,
    TypeDescriptor,
};
use pdx_parser::{CstKind, CstNode, ParsedFile};
use pdx_text::{LogicalPath, TextRange, TextSize};

/// A conservative semantic scope value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scope {
    /// No scope is known yet; later analysis must avoid cascading errors.
    Unknown,
    /// The root scope of a file.
    Root,
}

/// A conservative set of possible game scopes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopeValue {
    /// One or more statically known scope spellings.
    Known(Vec<String>),
    /// Lowering lacks enough information to determine the scope.
    Unknown,
    /// The rules prove that no scope is valid.
    Invalid,
}

/// Persistent scope registers at one semantic location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeState {
    /// Scope at the semantic root.
    pub root: ScopeValue,
    /// Current scope stack, with the active scope first.
    pub current: Vec<ScopeValue>,
    /// FROM registers, nearest first.
    pub from: Vec<ScopeValue>,
    /// PREV/previous registers, nearest first.
    pub previous: Vec<ScopeValue>,
}

impl ScopeState {
    fn initial(scope: ScopeValue) -> Self {
        Self { root: scope.clone(), current: vec![scope], from: Vec::new(), previous: Vec::new() }
    }
}

/// Cached semantic root context and initial scope for one source property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeFact {
    /// Exact key range that identifies the semantic root.
    pub range: TextRange,
    /// Semantic rule context, such as `effect` or `type:event`.
    pub context: String,
    /// Semantic parent path at this property after context resets and transparent wrappers.
    pub parent_path: Vec<String>,
    /// Initial persistent scope registers for this root.
    pub state: ScopeState,
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

/// One parser recovery node retained instead of being silently discarded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirUnknownConstruct {
    /// Exact source range occupied by the recovery node.
    pub range: TextRange,
}

/// One `[[name] ... ]` or `[[!name] ... ]` conditional parameter block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirParameterConditional {
    /// Parameter spelling without the optional `!`.
    pub name: String,
    /// Whether the block applies when the parameter is undefined.
    pub negated: bool,
    /// Full conditional block range.
    pub range: TextRange,
    /// Exact condition range, including `!` when present.
    pub condition_range: TextRange,
    /// Exact parameter-name range, excluding `!`.
    pub name_range: TextRange,
}

/// One parameter inferred within a scripted definition block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirParameterDefinition {
    /// Parameter spelling without delimiters.
    pub name: String,
    /// First occurrence that establishes the inferred parameter.
    pub range: TextRange,
    /// Exact range of the parameter name.
    pub name_range: TextRange,
    /// Top-level scripted definition that owns this local parameter.
    pub owner_range: TextRange,
    /// Delimiter used by substitution occurrences.
    pub delimiter: char,
}

/// The syntax form that uses a local parameter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HirParameterReferenceKind {
    /// A delimited substitution such as `$NAME$`.
    Substitution,
    /// A conditional block such as `[[NAME] ... ]`.
    Conditional,
}

/// One use of a parameter within a scripted definition block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirParameterReference {
    /// Referenced parameter spelling without delimiters or `!`.
    pub name: String,
    /// Full substitution or condition range.
    pub range: TextRange,
    /// Exact range of the parameter name.
    pub name_range: TextRange,
    /// Top-level scripted definition that owns this local reference.
    pub owner_range: TextRange,
    /// Source syntax form.
    pub kind: HirParameterReferenceKind,
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
    scope_facts: Vec<ScopeFact>,
    unknown_constructs: Vec<HirUnknownConstruct>,
    parameter_conditionals: Vec<HirParameterConditional>,
    parameter_definitions: Vec<HirParameterDefinition>,
    parameter_references: Vec<HirParameterReference>,
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

    /// Returns cached semantic-root scope facts in source order.
    #[must_use]
    pub fn scope_facts(&self) -> &[ScopeFact] {
        &self.scope_facts
    }

    /// Finds a cached scope fact in logarithmic time by exact key range and context.
    #[must_use]
    pub fn scope_fact(&self, range: TextRange, context: &str) -> Option<&ScopeFact> {
        let first = self.scope_facts.partition_point(|fact| fact.range < range);
        self.scope_facts[first..]
            .iter()
            .take_while(|fact| fact.range == range)
            .find(|fact| fact.context.eq_ignore_ascii_case(context))
    }

    /// Finds the cached scope fact at an exact key range regardless of semantic context.
    #[must_use]
    pub fn scope_fact_at(&self, range: TextRange) -> Option<&ScopeFact> {
        let first = self.scope_facts.partition_point(|fact| fact.range < range);
        self.scope_facts.get(first).filter(|fact| fact.range == range)
    }

    /// Returns parser recovery constructs in source order.
    #[must_use]
    pub fn unknown_constructs(&self) -> &[HirUnknownConstruct] {
        &self.unknown_constructs
    }

    /// Returns conditional parameter blocks in source order.
    #[must_use]
    pub fn parameter_conditionals(&self) -> &[HirParameterConditional] {
        &self.parameter_conditionals
    }

    /// Returns inferred local parameter definitions in source order.
    #[must_use]
    pub fn parameter_definitions(&self) -> &[HirParameterDefinition] {
        &self.parameter_definitions
    }

    /// Iterates inferred definitions owned by one top-level scripted definition.
    pub fn parameter_definitions_for_owner(
        &self,
        owner_range: TextRange,
    ) -> impl Iterator<Item = &HirParameterDefinition> {
        let first = self
            .parameter_definitions
            .partition_point(|definition| definition.range.start() < owner_range.start());
        self.parameter_definitions[first..]
            .iter()
            .take_while(move |definition| definition.range.start() < owner_range.end())
            .filter(move |definition| definition.owner_range == owner_range)
    }

    /// Returns local parameter uses in source order.
    #[must_use]
    pub fn parameter_references(&self) -> &[HirParameterReference] {
        &self.parameter_references
    }

    /// Finds the local parameter occurrence containing an exact source position.
    #[must_use]
    pub fn parameter_reference_at(&self, position: TextSize) -> Option<&HirParameterReference> {
        let first = self
            .parameter_references
            .partition_point(|reference| reference.range.end() <= position);
        self.parameter_references.get(first).filter(|reference| {
            position >= reference.range.start() && position < reference.range.end()
        })
    }

    /// Iterates parameter uses owned by one top-level scripted definition.
    pub fn parameter_references_for_owner(
        &self,
        owner_range: TextRange,
    ) -> impl Iterator<Item = &HirParameterReference> {
        let first = self
            .parameter_references
            .partition_point(|reference| reference.range.start() < owner_range.start());
        self.parameter_references[first..]
            .iter()
            .take_while(move |reference| reference.range.start() < owner_range.end())
            .filter(move |reference| reference.owner_range == owner_range)
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
    let (properties, localisation_entries, bare_values, unknown_constructs, parameter_conditionals) = {
        let mut collector = FactCollector::new(&syntax);
        collector.collect(syntax.root(), true, false, &[]);
        (
            collector.properties,
            collector.localisation_entries,
            collector.bare_values,
            collector.unknown_constructs,
            collector.parameter_conditionals,
        )
    };
    let (definitions, references) = lower_semantics(
        &properties,
        &localisation_entries,
        &bare_values,
        logical_path,
        rules,
        profile,
    );
    let scope_facts = lower_scope_facts(&properties, logical_path, rules, profile);
    let (parameter_definitions, parameter_references) =
        lower_parameters(&syntax, &properties, &parameter_conditionals, logical_path, profile);
    HirFile {
        syntax,
        scope: Scope::Unknown,
        properties,
        localisation_entries,
        bare_values,
        definitions,
        references,
        scope_facts,
        unknown_constructs,
        parameter_conditionals,
        parameter_definitions,
        parameter_references,
    }
}

fn lower_parameters(
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
        for token in
            syntax.tokens().iter().filter(|token| token.kind() == pdx_parser::TokenKind::Bare)
        {
            let Some(raw) = syntax.text(token.range()) else { continue };
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
    definitions.push(HirParameterDefinition { name, range, name_range, owner_range, delimiter });
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

fn lower_scope_facts(
    properties: &[HirProperty],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> Vec<ScopeFact> {
    let Some(profile) = profile else { return Vec::new() };
    let property_children = property_children(properties);
    let mut facts = Vec::new();
    for (property_index, property) in
        properties.iter().enumerate().filter(|(_, property)| property.top_level)
    {
        let Some(context) = semantic_root_context(rules, logical_path, &property.key) else {
            continue;
        };
        facts.push(ScopeFact {
            range: property.key_range,
            state: initial_scope_state(rules, profile, &context, &property.key),
            context: context.clone(),
            parent_path: Vec::new(),
        });
        let initial = facts.last().expect("root fact was just inserted").state.clone();
        let Some(type_name) = context.strip_prefix("type:") else {
            lower_nested_scope_facts(
                properties,
                &property_children,
                property_index,
                rules,
                profile,
                &context,
                &[],
                &initial,
                &mut facts,
            );
            continue;
        };
        let skip_root =
            rules.model().semantic.type_descriptors.get(type_name).is_some_and(|descriptor| {
                descriptor.skip_root_paths.iter().any(|path| {
                    path.first().is_some_and(|key| {
                        key.eq_ignore_ascii_case("any") || key.eq_ignore_ascii_case(&property.key)
                    })
                })
            });
        if !skip_root {
            lower_nested_scope_facts(
                properties,
                &property_children,
                property_index,
                rules,
                profile,
                &context,
                &[],
                &initial,
                &mut facts,
            );
            continue;
        }
        for &child_index in &property_children[property_index] {
            let child = &properties[child_index];
            let child_state = initial_scope_state(rules, profile, &context, &child.key);
            facts.push(ScopeFact {
                range: child.key_range,
                state: child_state.clone(),
                context: context.clone(),
                parent_path: Vec::new(),
            });
            lower_nested_scope_facts(
                properties,
                &property_children,
                child_index,
                rules,
                profile,
                &context,
                &[],
                &child_state,
                &mut facts,
            );
        }
    }
    facts.sort_by_key(|fact| fact.range);
    facts
}

fn property_children(properties: &[HirProperty]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); properties.len()];
    let mut ancestors = Vec::<usize>::new();
    for (index, property) in properties.iter().enumerate() {
        while ancestors.last().is_some_and(|ancestor_index| {
            let ancestor = &properties[*ancestor_index];
            property.path.len() <= ancestor.path.len()
                || !property.path.starts_with(&ancestor.path)
                || !range_within(property.range, ancestor.range)
        }) {
            ancestors.pop();
        }
        if let Some(&parent_index) = ancestors.last()
            && property.path.len() == properties[parent_index].path.len().saturating_add(1)
        {
            children[parent_index].push(index);
        }
        ancestors.push(index);
    }
    children
}

#[allow(clippy::too_many_arguments)]
fn lower_nested_scope_facts(
    properties: &[HirProperty],
    property_children: &[Vec<usize>],
    parent_index: usize,
    rules: &RuleSet,
    profile: &GameProfile,
    context: &str,
    parent_path: &[String],
    state: &ScopeState,
    facts: &mut Vec<ScopeFact>,
) {
    for &property_index in &property_children[parent_index] {
        let property = &properties[property_index];
        let matching = rules
            .exact_semantic_rules(&property.key)
            .filter(|rule| {
                (rule.context.eq_ignore_ascii_case(context)
                    || context.strip_prefix("type:").is_some_and(|type_name| {
                        rule.context.eq_ignore_ascii_case(&format!("root:{type_name}"))
                    }))
                    && paths_equal(&rule.parent_path, parent_path)
                    && matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause)
                    && scope_allows(profile, state, rule)
            })
            .collect::<Vec<_>>();
        facts.push(ScopeFact {
            range: property.key_range,
            context: context.to_owned(),
            parent_path: parent_path.to_vec(),
            state: state.clone(),
        });
        let transparent = profile.is_transparent_scope_wrapper(&property.key);
        let Some(rule) = statically_selected_transition(
            &matching,
            properties,
            property_children,
            property_index,
            rules,
            context,
            parent_path,
            transparent,
        ) else {
            continue;
        };
        let (next_context, next_path) =
            transition_destination(rule, context, parent_path, &property.key, transparent);
        let next_state = child_scope_state(state, rule, rules, profile);
        lower_nested_scope_facts(
            properties,
            property_children,
            property_index,
            rules,
            profile,
            &next_context,
            &next_path,
            &next_state,
            facts,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn statically_selected_transition<'rule>(
    matching: &[&'rule SemanticRule],
    properties: &[HirProperty],
    property_children: &[Vec<usize>],
    property_index: usize,
    rules: &RuleSet,
    context: &str,
    parent_path: &[String],
    transparent: bool,
) -> Option<&'rule SemanticRule> {
    if let Some(rule) = equivalent_transition(matching) {
        return Some(rule);
    }
    let children = &property_children[property_index];
    if children.is_empty() {
        return None;
    }
    let property = &properties[property_index];
    let possible = matching
        .iter()
        .copied()
        .filter(|candidate| {
            let (child_context, child_path) =
                transition_destination(candidate, context, parent_path, &property.key, transparent);
            children.iter().all(|child_index| {
                child_key_may_match(
                    rules,
                    &child_context,
                    &child_path,
                    &properties[*child_index].key,
                )
            })
        })
        .collect::<Vec<_>>();
    equivalent_transition(&possible)
}

fn transition_destination(
    rule: &SemanticRule,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent: bool,
) -> (String, Vec<String>) {
    rule.child_context.as_deref().map_or_else(
        || {
            let mut path = parent_path.to_vec();
            if !transparent {
                path.push(property_key.to_owned());
            }
            (context.to_owned(), path)
        },
        |child| (child.to_owned(), Vec::new()),
    )
}

fn child_key_may_match(rules: &RuleSet, context: &str, parent_path: &[String], key: &str) -> bool {
    let mut dynamic_matcher = false;
    let root_context = context.strip_prefix("type:").map(|type_name| format!("root:{type_name}"));
    for lookup_context in std::iter::once(context).chain(root_context.as_deref()) {
        for rule in rules.semantic_rules_for_context(lookup_context).filter(|rule| {
            paths_equal(&rule.parent_path, parent_path)
                && !matches!(rule.shape, RuleShape::LeafValue)
        }) {
            match &rule.key {
                KeyMatcher::Exact(expected) if expected.eq_ignore_ascii_case(key) => return true,
                KeyMatcher::Enum(enum_name) => {
                    let Some(values) =
                        rules.model().semantic.enum_values.iter().find_map(|(name, values)| {
                            name.eq_ignore_ascii_case(enum_name).then_some(values)
                        })
                    else {
                        dynamic_matcher = true;
                        continue;
                    };
                    if values.iter().any(|value| value.eq_ignore_ascii_case(key)) {
                        return true;
                    }
                }
                KeyMatcher::AnyScalar => return !key.is_empty(),
                KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => dynamic_matcher = true,
                KeyMatcher::Exact(_) => {}
            }
        }
    }
    dynamic_matcher
}

fn equivalent_transition<'rule>(matching: &[&'rule SemanticRule]) -> Option<&'rule SemanticRule> {
    let first = *matching.first()?;
    matching.iter().all(|candidate| transition_signature_matches(first, candidate)).then_some(first)
}

fn transition_signature_matches(left: &SemanticRule, right: &SemanticRule) -> bool {
    optional_text_matches(left.child_context.as_deref(), right.child_context.as_deref())
        && optional_text_matches(left.push_scope.as_deref(), right.push_scope.as_deref())
        && left.replace_scope.len() == right.replace_scope.len()
        && left.replace_scope.iter().all(|(left_register, left_scope)| {
            right.replace_scope.iter().any(|(right_register, right_scope)| {
                left_register.eq_ignore_ascii_case(right_register)
                    && left_scope.eq_ignore_ascii_case(right_scope)
            })
        })
        && right.replace_scope.iter().all(|(right_register, right_scope)| {
            left.replace_scope.iter().any(|(left_register, left_scope)| {
                left_register.eq_ignore_ascii_case(right_register)
                    && left_scope.eq_ignore_ascii_case(right_scope)
            })
        })
}

fn optional_text_matches(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

fn paths_equal(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn scope_allows(profile: &GameProfile, state: &ScopeState, rule: &SemanticRule) -> bool {
    rule.allowed_scopes.is_empty()
        || state.current.first().is_none_or(|current| match current {
            ScopeValue::Known(scopes) => rule.allowed_scopes.iter().any(|expected| {
                scopes.iter().any(|actual| profile.scopes_compatible(actual, expected))
            }),
            ScopeValue::Unknown => true,
            ScopeValue::Invalid => false,
        })
}

fn child_scope_state(
    parent: &ScopeState,
    rule: &SemanticRule,
    rules: &RuleSet,
    profile: &GameProfile,
) -> ScopeState {
    let mut child = parent.clone();
    if let Some(push_scope) = rule.push_scope.as_deref()
        && !push_scope.eq_ignore_ascii_case("any")
    {
        if let Some(current) = child.current.first().cloned() {
            child.previous.insert(0, current);
        }
        child.current.insert(0, ScopeValue::Known(vec![push_scope.to_owned()]));
    }
    for (register, value) in &rule.replace_scope {
        let value = resolve_scope_expression(&child, value, rules, profile);
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root = value,
            "this" => set_scope_register(&mut child.current, 0, value),
            _ => {
                if let Some(depth) = repeated_scope_register_depth(&register, "from") {
                    set_scope_register(&mut child.from, depth, value);
                } else if let Some(depth) = repeated_scope_register_depth(&register, "previous")
                    .or_else(|| repeated_scope_register_depth(&register, "prev"))
                {
                    set_scope_register(&mut child.previous, depth, value);
                }
            }
        }
    }
    child
}

fn resolve_scope_expression(
    state: &ScopeState,
    expression: &str,
    rules: &RuleSet,
    profile: &GameProfile,
) -> ScopeValue {
    if expression.contains('.') {
        let mut segments = expression.split('.');
        let Some(first) = segments.next() else {
            return ScopeValue::Unknown;
        };
        let mut value = resolve_scope_expression(state, first, rules, profile);
        for segment in segments {
            value = resolve_scope_link(&value, segment, rules, profile).1;
            if !matches!(value, ScopeValue::Known(_)) {
                break;
            }
        }
        return value;
    }

    let lowered = expression.to_ascii_lowercase().replace('_', "");
    if lowered == "root" {
        return state.root.clone();
    }
    if lowered == "this" {
        return state.current.first().cloned().unwrap_or(ScopeValue::Unknown);
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        return state.from.get(depth).cloned().unwrap_or(ScopeValue::Unknown);
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        return state.previous.get(depth).cloned().unwrap_or(ScopeValue::Unknown);
    }

    let current = state.current.first().unwrap_or(&ScopeValue::Unknown);
    let (link_expression, resolved) = resolve_scope_link(current, expression, rules, profile);
    if !matches!(resolved, ScopeValue::Unknown) {
        return resolved;
    }
    if expression.eq_ignore_ascii_case("any") || link_expression {
        ScopeValue::Unknown
    } else if profile.is_scope(expression) {
        ScopeValue::Known(vec![expression.to_owned()])
    } else {
        ScopeValue::Unknown
    }
}

fn resolve_scope_link(
    current: &ScopeValue,
    expression: &str,
    rules: &RuleSet,
    profile: &GameProfile,
) -> (bool, ScopeValue) {
    let mut targets = Vec::new();
    let mut link_expression = false;
    if let ScopeValue::Known(actual_scopes) = current {
        for rule in rules.exact_semantic_rules(expression) {
            if !matches!(rule.context.to_ascii_lowercase().as_str(), "effect" | "trigger") {
                continue;
            }
            link_expression = true;
            let allowed = rule.allowed_scopes.is_empty()
                || rule.allowed_scopes.iter().any(|expected| {
                    actual_scopes.iter().any(|actual| profile.scopes_compatible(actual, expected))
                });
            if !allowed {
                continue;
            }
            if let Some(target) =
                rule.push_scope.as_deref().filter(|target| !target.eq_ignore_ascii_case("any"))
                && !targets.iter().any(|known: &String| known.eq_ignore_ascii_case(target))
            {
                targets.push(target.to_owned());
            }
        }
    } else {
        link_expression = rules.exact_semantic_rules(expression).any(|rule| {
            matches!(rule.context.to_ascii_lowercase().as_str(), "effect" | "trigger")
                && rule.push_scope.is_some()
        });
    }
    if !targets.is_empty() {
        targets.sort_by_key(|target| target.to_ascii_lowercase());
        return (link_expression, ScopeValue::Known(targets));
    }
    (link_expression, ScopeValue::Unknown)
}

fn repeated_scope_register_depth(value: &str, token: &str) -> Option<usize> {
    let count = value.len().checked_div(token.len())?;
    if count > 0 && token.repeat(count) == value { Some(count - 1) } else { None }
}

fn set_scope_register(registers: &mut Vec<ScopeValue>, depth: usize, value: ScopeValue) {
    if registers.len() <= depth {
        registers.resize(depth + 1, ScopeValue::Unknown);
    }
    registers[depth] = value;
}

fn initial_scope_state(
    rules: &RuleSet,
    profile: &GameProfile,
    context: &str,
    root_key: &str,
) -> ScopeState {
    let scope = context
        .strip_prefix("type:")
        .and_then(|type_name| rules.model().semantic.type_root_scopes.get(type_name))
        .and_then(|roots| {
            roots
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(root_key))
                .map(|(_, scope)| scope.as_str())
        })
        .or_else(|| profile.root_scope(root_key))
        .map_or(ScopeValue::Unknown, |scope| ScopeValue::Known(vec![scope.to_owned()]));
    ScopeState::initial(scope)
}

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
                        key.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
                    }) || descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|root| {
                            root.eq_ignore_ascii_case("any") || root.eq_ignore_ascii_case(key)
                        })
                    })
                }))
                && semantic
                    .rules
                    .iter()
                    .any(|rule| rule.context.eq_ignore_ascii_case(&format!("root:{type_name}")))
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
                        key.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
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
    let Some(logical_path) = logical_path else { return true };
    let path = logical_path.as_str().replace('\\', "/").to_ascii_lowercase();
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
            && path.strip_prefix(&format!("{prefix}/")).is_some_and(|rest| rest.contains('/'))
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
            self.unknown_constructs.push(HirUnknownConstruct { range: node.range() });
        }
        if node.kind() == CstKind::ParameterBlock
            && let Some(condition) =
                node.children().iter().find(|child| child.kind() == CstKind::ParameterCondition)
            && let Some(raw) = self.syntax.text(condition.range()).map(str::trim)
        {
            let (negated, name) = raw.strip_prefix('!').map_or((false, raw), |name| (true, name));
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
    use super::{
        HirParameterReferenceKind, ScopeState, ScopeValue, lower, lower_with_profile,
        property_children, resolve_scope_expression,
    };
    use pdx_game::eu4::{bootstrap_rules, first_party_rules, profile};
    use pdx_rules::{GameProfile, RuleSet, RuleShape};
    use pdx_parser::{FileFormat, parse};
    use pdx_text::LogicalPath;

    #[test]
    fn lowering_retains_property_paths_scalars_and_top_level_identity() {
        let parsed =
            parse(FileFormat::Script, "root = { child = \"value\" nested = { leaf = yes } }\n");
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
    fn property_adjacency_preserves_duplicate_siblings_and_nested_children() {
        let hir = lower(
            parse(
                FileFormat::Script,
                concat!(
                    "root = { ",
                    "duplicate = { child = one } ",
                    "duplicate = { child = two nested = { leaf = three } } ",
                    "tail = yes",
                    " }\n",
                ),
            ),
            &RuleSet::empty(),
        );
        let children = property_children(hir.properties());
        let root = hir
            .properties()
            .iter()
            .position(|property| property.top_level && property.key == "root")
            .expect("root property");
        let root_keys = children[root]
            .iter()
            .map(|index| hir.properties()[*index].key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(root_keys, ["duplicate", "duplicate", "tail"]);

        let second_duplicate = children[root][1];
        let duplicate_keys = children[second_duplicate]
            .iter()
            .map(|index| hir.properties()[*index].key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(duplicate_keys, ["child", "nested"]);
        let nested = children[second_duplicate][1];
        assert_eq!(children[nested].len(), 1);
        assert_eq!(hir.properties()[children[nested][0]].key, "leaf");
    }

    #[test]
    fn lowering_retains_localisation_definition_ranges() {
        let parsed = parse(FileFormat::Localisation, "l_english:\n example_key:0 \"Example\"\n");
        let hir = lower(parsed, &RuleSet::empty());

        assert_eq!(hir.localisation_entries().len(), 1);
        assert_eq!(hir.localisation_entries()[0].name, "example_key");
        let entry = &hir.localisation_entries()[0];
        assert!(entry.range.start() <= entry.name_range.start());
        assert!(entry.name_range.end() <= entry.range.end());
    }

    #[test]
    fn lowering_retains_recovery_nodes_as_unknown_constructs() {
        let source = "root = { = broken good = yes }\n";
        let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());

        assert!(!hir.syntax().errors().is_empty());
        assert!(!hir.unknown_constructs().is_empty());
        assert!(hir.unknown_constructs().iter().all(|unknown| {
            unknown.range.end() <= u32::try_from(source.len()).unwrap_or(u32::MAX)
        }));
        assert!(hir.properties().iter().any(|property| property.key == "good"));
    }

    #[test]
    fn lowering_retains_parameter_conditionals_with_polarity() {
        let source = "[[enabled] value = yes ]\n[[!disabled] other = no ]\n";
        let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());

        assert_eq!(hir.parameter_conditionals().len(), 2);
        assert_eq!(hir.parameter_conditionals()[0].name, "enabled");
        assert!(!hir.parameter_conditionals()[0].negated);
        assert_eq!(hir.parameter_conditionals()[1].name, "disabled");
        assert!(hir.parameter_conditionals()[1].negated);
        assert!(hir.parameter_conditionals().iter().all(|conditional| {
            conditional.range.start() <= conditional.condition_range.start()
                && conditional.condition_range.end() <= conditional.range.end()
        }));
    }

    #[test]
    fn profile_lowering_associates_local_parameter_definitions_and_uses() {
        let source = concat!(
            "first = { value = $amount$ again = $amount$ ",
            "[[optional] enabled = yes ] }\n",
            "second = { value = $amount$ }\n",
        );
        let path =
            LogicalPath::parse("common/scripted_effects/parameters.txt").expect("logical path");
        let hir = lower_with_profile(
            parse(FileFormat::Script, source),
            &path,
            &bootstrap_rules(),
            &profile(),
        );

        assert_eq!(hir.parameter_definitions().len(), 3);
        assert_eq!(hir.parameter_references().len(), 4);
        assert_eq!(
            hir.parameter_definitions()
                .iter()
                .filter(|definition| definition.name == "amount")
                .count(),
            2
        );
        assert!(hir.parameter_definitions().iter().all(|definition| {
            hir.syntax().text(definition.name_range) == Some(definition.name.as_str())
                && definition.range.end() <= definition.owner_range.end()
        }));
        assert!(hir.parameter_references().iter().any(|reference| {
            reference.name == "optional" && reference.kind == HirParameterReferenceKind::Conditional
        }));
        assert_eq!(
            hir.parameter_references()
                .iter()
                .filter(|reference| reference.kind == HirParameterReferenceKind::Substitution)
                .count(),
            3
        );
        let first_owner = hir.parameter_definitions()[0].owner_range;
        assert_eq!(hir.parameter_definitions_for_owner(first_owner).count(), 2);
        assert_eq!(hir.parameter_references_for_owner(first_owner).count(), 3);
        let optional_position =
            u32::try_from(source.find("optional").expect("optional parameter")).expect("position");
        assert_eq!(
            hir.parameter_reference_at(optional_position).map(|reference| reference.name.as_str()),
            Some("optional")
        );
        assert!(hir.parameter_reference_at(first_owner.end()).is_none());
    }

    #[test]
    fn profile_aware_lowering_produces_shared_typed_definitions_and_references() {
        let rules = bootstrap_rules();
        let path = LogicalPath::parse("events/profile_hir.txt").expect("logical path");
        let source =
            "country_event = { id = profile.1 title = profile_title set_country_flag = seen }\n";

        let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile());

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

        let hir = lower_with_profile(parse(FileFormat::Script, source), &path, &rules, &profile);

        assert!(hir.definitions().is_empty());
        assert!(!hir.references().iter().any(|reference| reference.kind == "localisation"));
    }

    #[test]
    fn profile_lowering_caches_semantic_root_context_and_initial_scope() {
        let rules = first_party_rules().expect("embedded rules");
        let path = LogicalPath::parse("events/scope_hir.txt").expect("logical path");
        let hir = lower_with_profile(
            parse(
                FileFormat::Script,
                "country_event = { id = scope.1 immediate = { capital_scope = { add_base_tax = 1 } } }\n",
            ),
            &path,
            &rules,
            &profile(),
        );

        let fact = hir.scope_facts().first().expect("semantic root scope fact");
        assert_eq!(fact.context, "type:event");
        assert_eq!(fact.state.root, ScopeValue::Known(vec!["country".to_owned()]));
        assert_eq!(fact.state.current, vec![ScopeValue::Known(vec!["country".to_owned()])]);
        assert_eq!(hir.scope_fact(fact.range, "TYPE:EVENT"), Some(fact));
        let tax = hir
            .properties()
            .iter()
            .find(|property| property.key == "add_base_tax")
            .expect("nested province command");
        let tax_scope = hir
            .scope_facts()
            .iter()
            .find(|fact| fact.range == tax.key_range)
            .expect("nested transition fact");
        assert_eq!(
            tax_scope.state.current.first(),
            Some(&ScopeValue::Known(vec!["province".to_owned()]))
        );
        assert!(
            tax_scope.parent_path.is_empty(),
            "an explicit same-name effect child context resets the semantic path"
        );
    }

    #[test]
    fn equivalent_rule_alternatives_share_one_cached_transition() {
        let rules = first_party_rules().expect("embedded rules");
        let path = LogicalPath::parse("events/alternative_scope_hir.txt").expect("logical path");
        let hir = lower_with_profile(
            parse(
                FileFormat::Script,
                concat!(
                    "country_event = { immediate = { ",
                    "multiply_variable = { which = amount value = 2 }",
                    " } }\n",
                ),
            ),
            &path,
            &rules,
            &profile(),
        );

        let which = hir
            .properties()
            .iter()
            .find(|property| property.key == "which")
            .expect("alternative child");
        let fact = hir
            .scope_facts()
            .iter()
            .find(|fact| fact.range == which.key_range)
            .expect("equivalent alternatives should continue lowering");
        assert_eq!(
            fact.state.current.first(),
            Some(&ScopeValue::Known(vec!["country".to_owned()]))
        );

        let conflicting = lower_with_profile(
            parse(FileFormat::Script, "country_event = { mean_time_to_happen = { days = 30 } }\n"),
            &path,
            &rules,
            &profile(),
        );
        let days = conflicting
            .properties()
            .iter()
            .find(|property| property.key == "days")
            .expect("conflicting-transition child");
        let days_fact = conflicting
            .scope_facts()
            .iter()
            .find(|fact| fact.range == days.key_range)
            .expect("the child key statically eliminates the modifier-rule transition");
        assert_eq!(
            days_fact.context, "type:event",
            "the rules for `days` keep the event mean-time path"
        );
        assert_eq!(days_fact.parent_path, ["mean_time_to_happen"]);

        let modifier = lower_with_profile(
            parse(
                FileFormat::Script,
                concat!(
                    "country_event = { mean_time_to_happen = { ",
                    "modifier = { factor = 0.5 always = yes }",
                    " } }\n",
                ),
            ),
            &path,
            &rules,
            &profile(),
        );
        let modifier_property = modifier
            .properties()
            .iter()
            .find(|property| property.key == "modifier")
            .expect("modifier-rule child");
        let modifier_fact = modifier
            .scope_facts()
            .iter()
            .find(|fact| fact.range == modifier_property.key_range)
            .expect("the child key statically eliminates the event mean-time transition");
        assert_eq!(modifier_fact.context, "modifier_rule");
        assert!(modifier_fact.parent_path.is_empty());

        let empty = lower_with_profile(
            parse(FileFormat::Script, "country_event = { mean_time_to_happen = { } }\n"),
            &path,
            &rules,
            &profile(),
        );
        let empty_index = empty
            .properties()
            .iter()
            .position(|property| property.key == "mean_time_to_happen")
            .expect("empty ambiguous block");
        let children = super::property_children(empty.properties());
        let candidates = rules
            .exact_semantic_rules("mean_time_to_happen")
            .filter(|rule| {
                rule.context == "root:event"
                    && rule.parent_path.is_empty()
                    && matches!(rule.shape, RuleShape::Node)
            })
            .collect::<Vec<_>>();
        assert!(
            super::statically_selected_transition(
                &candidates,
                empty.properties(),
                &children,
                empty_index,
                &rules,
                "type:event",
                &[],
                false,
            )
            .is_none(),
            "an empty block must not guess between conflicting transitions"
        );
    }

    #[test]
    fn workspace_backed_child_keys_never_eliminate_a_transition_during_lowering() {
        let rules = first_party_rules().expect("embedded rules");
        assert!(
            super::child_key_may_match(
                &rules,
                "root:game_age",
                &["abilities".to_owned()],
                "workspace_defined_ability",
            ),
            "a type matcher can be satisfied by a later workspace definition"
        );
        assert!(
            super::child_key_may_match(
                &rules,
                "root:government_reform",
                &["custom_attributes".to_owned()],
                "workspace_defined_attribute",
            ),
            "a dynamic matcher can be satisfied by a later workspace value set"
        );
        assert!(
            !super::child_key_may_match(&rules, "modifier_rule", &[], "workspace_defined_ability",),
            "a context with only exact alternatives can still be ruled out"
        );
    }

    #[test]
    fn skipped_type_roots_still_cache_descendant_scope_facts() {
        let rules = first_party_rules().expect("embedded rules");
        let path = LogicalPath::parse("common/on_actions/scope_hir.txt").expect("logical path");
        let hir = lower_with_profile(
            parse(
                FileFormat::Script,
                concat!(
                    "on_harmonized_religiongroup = { ",
                    "random_events = { int = province_event.1 }",
                    " }\n",
                ),
            ),
            &path,
            &rules,
            &profile(),
        );

        let event = hir
            .properties()
            .iter()
            .find(|property| property.key == "int")
            .expect("random event entry");
        assert!(
            hir.scope_facts().iter().any(|fact| fact.range == event.key_range),
            "skip-root lowering must recurse below the selected semantic root"
        );
    }

    #[test]
    fn replace_scope_resolves_static_links_into_register_values() {
        assert_eq!(super::repeated_scope_register_depth("fromfrom", "from"), Some(1));
        assert_eq!(super::repeated_scope_register_depth("previous_owner", "previous"), None);

        let rules = first_party_rules().expect("embedded rules");
        let path = LogicalPath::parse("common/buildings/scope_hir.txt").expect("logical path");
        let hir = lower_with_profile(
            parse(
                FileFormat::Script,
                concat!("test_building = { ", "on_built = { cossack_infantry = FROM }", " }\n",),
            ),
            &path,
            &rules,
            &profile(),
        );

        let command = hir
            .properties()
            .iter()
            .find(|property| property.key == "cossack_infantry")
            .expect("nested effect");
        let fact = hir.scope_fact(command.key_range, "effect").expect("effect scope fact");
        assert_eq!(
            fact.state.current.first(),
            Some(&ScopeValue::Known(vec!["province".to_owned()]))
        );
        assert_eq!(fact.state.from.first(), Some(&ScopeValue::Known(vec!["country".to_owned()])));

        let province = ScopeValue::Known(vec!["province".to_owned()]);
        let state = ScopeState::initial(province.clone());
        assert_eq!(
            resolve_scope_expression(&state, "OwNeR.CAPITAL_SCOPE", &rules, &profile()),
            province
        );
        assert_eq!(
            resolve_scope_expression(&state, "owner.missing_link", &rules, &profile()),
            ScopeValue::Unknown
        );

        let mut invalid_register_rule = rules.model().semantic.rules[0].clone();
        invalid_register_rule.push_scope = None;
        invalid_register_rule.replace_scope = vec![
            ("from_owner".to_owned(), "country".to_owned()),
            ("previous_owner".to_owned(), "country".to_owned()),
        ];
        let unchanged =
            super::child_scope_state(&state, &invalid_register_rule, &rules, &profile());
        assert!(unchanged.from.is_empty());
        assert!(unchanged.previous.is_empty());
    }
}
