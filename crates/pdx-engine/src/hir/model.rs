//! HIR data model and read-only accessors.

use std::sync::Arc;

use pdx_parser::ParsedFile;
use pdx_text::{TextRange, TextSize};

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
    pub(crate) fn initial(scope: ScopeValue) -> Self {
        Self {
            root: scope.clone(),
            current: vec![scope],
            from: Vec::new(),
            previous: Vec::new(),
        }
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
    /// A localisation value selected by a first-party semantic rule.
    Semantic,
    /// A required type-instance localisation mapping expanded from a first-party template.
    DerivedLocalisation,
    /// A conservative bare value associated with the file category.
    Category,
}

/// A lowered file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFile {
    pub(super) syntax: Arc<ParsedFile>,
    pub(super) scope: Scope,
    pub(super) properties: Vec<HirProperty>,
    pub(super) localisation_entries: Vec<HirLocalisationEntry>,
    pub(super) bare_values: Vec<HirScalar>,
    pub(super) definitions: Vec<HirDefinition>,
    pub(super) references: Vec<HirReference>,
    pub(super) scope_facts: Vec<ScopeFact>,
    pub(super) unknown_constructs: Vec<HirUnknownConstruct>,
    pub(super) parameter_conditionals: Vec<HirParameterConditional>,
    pub(super) parameter_definitions: Vec<HirParameterDefinition>,
    pub(super) parameter_references: Vec<HirParameterReference>,
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
        self.scope_facts
            .get(first)
            .filter(|fact| fact.range == range)
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
