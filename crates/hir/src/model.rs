//! HIR data model and read-only accessors.

use std::sync::Arc;

use parser::ParsedFile;
use rules::ir::{FieldId, SchemaId, SubtypeSet};
use text::{TextRange, TextSize};

/// A conservative semantic scope value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scope {
    /// No scope is known yet; later analysis must avoid cascading errors.
    Unknown,
    /// The root scope of a file.
    Root,
}

/// A conservative set of possible game scopes.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ScopeValue {
    /// One or more statically known scope spellings. The list is shared
    /// (`ScopeState` clones once per scope fact, so the spellings themselves
    /// must not reallocate per clone).
    Known(std::sync::Arc<[std::sync::Arc<str>]>),
    /// Lowering lacks enough information to determine the scope.
    Unknown,
    /// The rules prove that no scope is valid.
    Invalid,
}

impl ScopeValue {
    /// A single known scope spelling.
    #[must_use]
    pub fn known_single(name: &str) -> Self {
        Self::Known(std::sync::Arc::from([std::sync::Arc::from(name)]))
    }

    /// Known scope spellings from owned strings.
    #[must_use]
    pub fn known(names: impl IntoIterator<Item = String>) -> Self {
        Self::Known(
            names
                .into_iter()
                .map(std::sync::Arc::from)
                .collect::<std::sync::Arc<[_]>>(),
        )
    }
}

/// Persistent scope registers at one semantic location.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
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
    pub fn initial(scope: ScopeValue) -> Self {
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
    /// Scope registers after this property applies its statically selected transition, when the
    /// lowering can prove one. `None` means that the property is not a known transition or has
    /// competing alternatives; consumers should keep the scope conservative in that case.
    pub transition: Option<ScopeState>,
}

/// A schema active over one block (or the complete document root).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaFact {
    /// Block value range, or whole-file range for the root schema.
    pub range: TextRange,
    /// Compiled schema selected by Rules IR.
    pub schema: SchemaId,
    /// Subtypes proved for this block's owning symbol instance.
    pub subtypes: SubtypeSet,
    /// Scope and register state at this block.
    pub state: ScopeState,
}

/// The Rules IR field candidates selected for one property key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldFact {
    /// Exact property key range.
    pub range: TextRange,
    /// Parent schema used for lookup.
    pub schema: SchemaId,
    /// Applicable field overloads or pattern candidates.
    pub fields: Vec<FieldId>,
    /// Subtypes proved at the property.
    pub subtypes: SubtypeSet,
}

/// One scalar value attached directly to a property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirScalar {
    /// Logical scalar text; quoted strings decode quote and backslash escapes once.
    pub value: String,
    /// Exact source range including quotes when present.
    pub range: TextRange,
    /// Whether the value was written as a quoted string literal.
    pub quoted: bool,
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
    /// Operator spelling, such as `=` or `!=`, when recovered by the parser.
    pub operator: Option<String>,
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

/// Attribute-key summary retained for one profile-interpreted definition whose
/// rule asked for retention. Keys are the body's direct property keys, as
/// written, in source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionAttributes {
    /// Dynamic symbol kind matching the HIR definition.
    pub kind: Arc<str>,
    /// Definition name as written in source.
    pub name: String,
    /// Full range of the owning symbol definition.
    pub definition_range: TextRange,
    /// Direct body property keys in source order, as written.
    pub attribute_keys: Vec<Arc<str>>,
    /// Proven subtypes of this symbol instance when lowered from Rules IR.
    pub subtypes: Vec<Arc<str>>,
}

/// One profile-interpreted symbol definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirDefinition {
    /// Stable workspace symbol kind.
    pub kind: Arc<str>,
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
    pub kind: Arc<str>,
    /// Referenced symbol spelling.
    pub name: String,
    /// Exact source range of the reference.
    pub range: TextRange,
    /// Interpretation layer that emitted this reference.
    pub origin: HirReferenceOrigin,
    /// Required subtype when the target is a qualified Rules IR type reference.
    pub subtype: Option<Arc<str>>,
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
    /// A delimited substitution in a scalar value such as `value = $NAME$`.
    Substitution,
    /// A delimited substitution that supplies a property key or scope register.
    KeySubstitution,
    /// A delimited substitution embedded in quoted script text.
    OpaqueTextSubstitution,
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

pub use rules::template::{
    Template, TemplateConditional, TemplateFragment, TemplateItem, TemplateProperty, TemplateToken,
    TemplateValue,
};

/// The interpretation layer that emitted a HIR reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HirReferenceOrigin {
    /// A precise property matcher from the selected game profile.
    Profile,
    /// A value selected by a first-party semantic rule.
    Semantic,
    /// A workspace-symbol value selected by a first-party semantic rule.
    ///
    /// These references are kept separate from ordinary semantic values because the analysis
    /// layer can omit unresolved typed values from its navigation view while retaining the
    /// semantic validator's more precise value diagnostic.
    SemanticTyped,
    /// A concrete scripted-effect or scripted-trigger invocation selected by dynamic-definition metadata.
    DynamicDefinition,
    /// A required type-instance localisation mapping expanded from a first-party template.
    DerivedLocalisation,
    /// A type-instance icon mapping expanded from a first-party icon binding.
    DerivedSprite,
    /// A conservative bare value associated with the file category.
    Category,
}

/// A lowered file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFile {
    pub(super) analysis_coverage: crate::analysis::AnalysisCoverage,
    pub(super) overload_facts: Vec<crate::block_checking::OverloadFact>,
    pub(super) syntax: Arc<ParsedFile>,
    pub(super) scope: Scope,
    pub(super) properties: Vec<HirProperty>,
    pub(super) localisation_entries: Vec<HirLocalisationEntry>,
    pub(super) bare_values: Vec<HirScalar>,
    pub(super) definitions: Vec<HirDefinition>,
    pub(super) references: Vec<HirReference>,
    pub(super) scope_facts: Vec<ScopeFact>,
    pub(super) schema_facts: Vec<SchemaFact>,
    pub(super) field_facts: Vec<FieldFact>,
    pub(super) unknown_constructs: Vec<HirUnknownConstruct>,
    pub(super) parameter_conditionals: Vec<HirParameterConditional>,
    pub(super) parameter_definitions: Vec<HirParameterDefinition>,
    pub(super) parameter_references: Vec<HirParameterReference>,
    pub(super) dynamic_templates: Vec<Template>,
    pub(super) definition_attributes: Vec<DefinitionAttributes>,
    pub(super) uses_ir: bool,
    pub(super) symbol_facts_dependency: bool,
    pub(super) binding_references: Vec<HirReference>,
    pub(super) runtime_parameter_guards: Vec<(TextRange, Option<TextRange>)>,
}

impl HirFile {
    /// Correlated whole-container overload selections and unresolved alternatives.
    pub fn overload_facts(&self) -> &[crate::block_checking::OverloadFact] {
        &self.overload_facts
    }

    /// Coverage of generated semantic facts and represented Template declarations.
    pub fn analysis_coverage(&self) -> &crate::analysis::AnalysisCoverage {
        &self.analysis_coverage
    }

    /// Retains a related transaction frontier without discarding independent evidence.
    pub fn merge_analysis_coverage(&mut self, coverage: &crate::analysis::AnalysisCoverage) {
        self.analysis_coverage.merge(coverage);
    }

    /// Removes facts whose dispatch depends on a failed discovery transaction.
    /// Syntax and unconditional declarations remain available for editing.
    pub fn discard_facts_in_ranges(&mut self, ranges: &[text::TextRange]) {
        let inside = |range: text::TextRange| {
            ranges
                .iter()
                .any(|parent| parent.start() <= range.start() && range.end() <= parent.end())
        };
        self.definitions.retain(|def| !inside(def.range));
        self.references.retain(|reference| !inside(reference.range));
        self.binding_references
            .retain(|reference| !inside(reference.range));
        self.definition_attributes
            .retain(|attrs| !inside(attrs.definition_range));
        self.dynamic_templates
            .retain(|template| !inside(template.definition_range));
    }

    /// Whether lowering consulted workspace symbols, including missing symbols.
    /// Files without such reads can reuse their shard during symbol-fact replay.
    #[must_use]
    pub const fn depends_on_symbol_facts(&self) -> bool {
        self.symbol_facts_dependency
    }

    /// Whether this file was lowered through the compiled Rules IR path.
    #[must_use]
    pub const fn uses_ir(&self) -> bool {
        self.uses_ir
    }

    /// Complete trait binding references for hover, including optional mappings.
    #[must_use]
    pub fn binding_references_for_hover(&self) -> &[HirReference] {
        &self.binding_references
    }
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

    /// Finds the first property with this exact key range in source order.
    #[must_use]
    pub fn property_at_key_range(&self, range: TextRange) -> Option<&HirProperty> {
        let index = self
            .properties
            .partition_point(|property| property.key_range < range);
        self.properties
            .get(index)
            .filter(|property| property.key_range == range)
    }

    /// Returns properties fully contained in `range`, in source order.
    /// Source-ordered starts bound the search to this part of the document.
    pub fn properties_in_range(
        &self,
        range: TextRange,
    ) -> impl DoubleEndedIterator<Item = &HirProperty> + Clone {
        let first = self
            .properties
            .partition_point(|property| property.range.start() < range.start());
        let last = self
            .properties
            .partition_point(|property| property.range.start() <= range.end());
        self.properties[first..last]
            .iter()
            .filter(move |property| property.range.end() <= range.end())
    }

    /// Returns localisation definitions in source order.
    #[must_use]
    pub fn localisation_entries(&self) -> &[HirLocalisationEntry] {
        &self.localisation_entries
    }

    /// Returns scalar value tokens, including quoted list items, that are not property keys.
    #[must_use]
    pub fn bare_values(&self) -> &[HirScalar] {
        &self.bare_values
    }

    /// Returns profile-interpreted definitions in deterministic source order.
    #[must_use]
    pub fn definitions(&self) -> &[HirDefinition] {
        &self.definitions
    }

    /// Returns retained attribute-key summaries in deterministic source order.
    #[must_use]
    pub fn definition_attributes(&self) -> &[DefinitionAttributes] {
        &self.definition_attributes
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

    /// Returns all compiled schema facts in source order.
    #[must_use]
    pub fn schema_facts(&self) -> &[SchemaFact] {
        &self.schema_facts
    }

    /// Finds the most deeply nested schema fact containing `position`.
    #[must_use]
    pub fn schema_at(&self, position: TextSize) -> Option<&SchemaFact> {
        self.schema_facts
            .iter()
            .filter(|fact| fact.range.start() <= position && position <= fact.range.end())
            .min_by_key(|fact| fact.range.len())
    }

    /// Returns field facts, including keys mapped from quoted scripts.
    #[must_use]
    pub fn field_facts(&self) -> &[FieldFact] {
        &self.field_facts
    }

    /// Finds Rules IR field candidates for an exact property key range.
    #[must_use]
    pub fn field_fact_at(&self, range: TextRange) -> Option<&FieldFact> {
        self.field_facts
            .binary_search_by_key(&range, |fact| fact.range)
            .ok()
            .map(|index| &self.field_facts[index])
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

    /// Returns reusable dynamic-definition templates in definition source order.
    #[must_use]
    pub fn dynamic_templates(&self) -> &[Template] {
        &self.dynamic_templates
    }

    /// Finds the template belonging to one exact definition range and kind/name identity.
    #[must_use]
    pub fn dynamic_template(
        &self,
        kind: &str,
        name: &str,
        definition_range: TextRange,
    ) -> Option<&Template> {
        self.dynamic_templates.iter().find(|template| {
            template.definition_range == definition_range
                && template.kind.eq_ignore_ascii_case(kind)
                && template.name.eq_ignore_ascii_case(name)
        })
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

    /// Returns whether a caller must provide one inferred local parameter.
    ///
    /// The compact signature can only express unconditional presence. A substitution is therefore
    /// optional when every value/key/text use is protected by an explicit parameter conditional
    /// or by the body of an ordinary `if`/`else_if`/`else` runtime branch; branch `limit` values
    /// remain required because the game must evaluate them. A parameter used only as a condition
    /// can always be omitted by the caller.
    #[must_use]
    pub fn parameter_is_required(&self, owner_range: TextRange, name: &str) -> bool {
        let owner_conditionals = self
            .parameter_conditionals
            .iter()
            .filter(|conditional| {
                conditional.range.start() >= owner_range.start()
                    && conditional.range.end() <= owner_range.end()
            })
            .collect::<Vec<_>>();
        for reference in self
            .parameter_references_for_owner(owner_range)
            .filter(|reference| {
                reference.name.eq_ignore_ascii_case(name)
                    && reference.kind != HirParameterReferenceKind::Conditional
            })
        {
            let guarded = owner_conditionals.iter().any(|conditional| {
                reference.range.start() >= conditional.range.start()
                    && reference.range.end() <= conditional.range.end()
            });
            if !guarded
                && !self.parameter_reference_is_runtime_guarded(reference)
                && self.parameter_reference_occupies_token(reference)
                && !self.parameter_reference_is_same_named_value(reference)
            {
                return true;
            }
        }
        false
    }

    fn parameter_reference_is_runtime_guarded(&self, reference: &HirParameterReference) -> bool {
        self.runtime_parameter_guards.iter().any(|(branch, guard)| {
            branch.start() <= reference.range.start()
                && reference.range.end() <= branch.end()
                && guard.is_none_or(|guard| {
                    !(guard.start() <= reference.range.start()
                        && reference.range.end() <= guard.end())
                })
        })
    }

    fn parameter_reference_occupies_token(&self, reference: &HirParameterReference) -> bool {
        fn containing_token(
            node: parser::CstNode<'_>,
            range: TextRange,
        ) -> Option<parser::CstNode<'_>> {
            if range.start() < node.range().start() || range.end() > node.range().end() {
                return None;
            }
            node.children()
                .find_map(|child| containing_token(child, range))
                .or_else(|| {
                    matches!(
                        node.kind(),
                        parser::CstKind::Key
                            | parser::CstKind::BareValue
                            | parser::CstKind::QuotedString
                    )
                    .then_some(node)
                })
        }

        let Some(token) = containing_token(self.syntax.root(), reference.range) else {
            return true;
        };
        let Some(raw) = self.syntax.text(token.range()).map(str::trim) else {
            return true;
        };
        let content = raw
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(raw);
        content.eq_ignore_ascii_case(&format!("${}$", reference.name))
    }

    fn parameter_reference_is_same_named_value(&self, reference: &HirParameterReference) -> bool {
        fn containing_property(
            node: parser::CstNode<'_>,
            range: TextRange,
        ) -> Option<parser::CstNode<'_>> {
            if range.start() < node.range().start() || range.end() > node.range().end() {
                return None;
            }
            node.children()
                .find_map(|child| containing_property(child, range))
                .or_else(|| (node.kind() == parser::CstKind::Property).then_some(node))
        }

        containing_property(self.syntax.root(), reference.range)
            .and_then(|property| {
                property
                    .children()
                    .find(|child| child.kind() == parser::CstKind::Key)
            })
            .and_then(|key| self.syntax.text(key.range()))
            .is_some_and(|key| key.trim().eq_ignore_ascii_case(&reference.name))
    }
}
