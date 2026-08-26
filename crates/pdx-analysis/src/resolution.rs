use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use pdx_engine::hir::{HirFile, HirReference, HirReferenceOrigin};
use pdx_engine::{
    AnalysisSnapshot, Definition, DocumentId, DocumentSource, Reference, SourceFileId,
};
use pdx_rules::{KeyMatcher, RuleShape, SymbolResolutionPolicy};
use pdx_text::{LogicalPath, TextRange, TextSize};
#[cfg(test)]
use std::cell::Cell;

use crate::completion::{SemanticCompletionContext, infer_macro_quoted_script_constraints};
use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::semantic::*;
use crate::support::*;
use crate::types::*;

#[derive(Clone, Debug)]
pub(crate) struct DefinitionInfo {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) symbol: Symbol,
    pub(crate) document: Option<DocumentId>,
    pub(crate) file: Option<SourceFileId>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReferenceInternal {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) range: TextRange,
    pub(crate) document: Option<DocumentId>,
    pub(crate) file: Option<SourceFileId>,
    pub(crate) path: Option<LogicalPath>,
}

impl ReferenceInternal {
    pub(crate) fn location(&self) -> Location {
        Location {
            document: self.document.clone(),
            file: self.file,
            path: self.path.clone(),
            range: self.range,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SemanticWorkspace {
    pub(crate) definitions: Vec<DefinitionInfo>,
    pub(crate) references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug)]
pub(crate) struct SemanticFile {
    pub(crate) definitions: Vec<DefinitionInfo>,
    pub(crate) references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolutionDefinition {
    pub(crate) location: Location,
    pub(crate) selection_range: TextRange,
    pub(crate) priority: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RenameTarget {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) cursor_range: TextRange,
    pub(crate) definition: ResolutionDefinition,
}

pub(crate) enum Resolution {
    Unique(ResolutionDefinition),
    Ambiguous,
    Missing,
}

pub(crate) fn semantic_data(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> SemanticFile {
    uncancelled(semantic_data_with_cancellation(
        snapshot,
        input,
        &CancellationToken::new(),
    ))
}

pub(crate) fn semantic_data_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<SemanticFile, Cancelled> {
    cancellation.checkpoint()?;
    // Overlay documents are re-extracted by every query (diagnostics, hover, completion,
    // navigation). The result only depends on the immutable snapshot, so cache it per
    // (revision, document) and share it across all worker threads observing that revision.
    let Some(document) = input.document.as_ref() else {
        return semantic_data_with_cancellation_uncached(snapshot, input, cancellation);
    };
    let revision = snapshot.revision();
    let key = document.as_str();
    if let Some(cached) = snapshot.query_cache().get::<SemanticFile>(revision, key) {
        return Ok((*cached).clone());
    }
    let data = semantic_data_with_cancellation_uncached(snapshot, input, cancellation)?;
    snapshot
        .query_cache()
        .insert(revision, key.to_owned(), Arc::new(data.clone()));
    Ok(data)
}

fn semantic_data_with_cancellation_uncached(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<SemanticFile, Cancelled> {
    let mut data = SemanticFile {
        definitions: Vec::new(),
        references: Vec::new(),
    };
    let Some(hir) = input.hir.as_deref() else {
        collect_quoted_semantics(snapshot, input, &mut data, cancellation)?;
        return Ok(data);
    };
    // The inactive-range set is only consulted for Semantic-origin references; skip building it
    // entirely when this file has none, keeping semantic_data O(references + definitions).
    let has_semantic_references = hir
        .references()
        .iter()
        .any(|reference| reference.origin == HirReferenceOrigin::Semantic);
    let inactive_semantic_references = if has_semantic_references {
        inactive_semantic_reference_ranges(snapshot, hir)
    } else {
        BTreeSet::new()
    };
    for definition in hir.definitions() {
        cancellation.checkpoint()?;
        data.definitions.push(make_definition(
            input,
            &definition.kind,
            definition.name.clone(),
            definition.range,
            definition.selection_range,
        ));
    }
    for reference in hir
        .references()
        .iter()
        .filter(|reference| {
            matches!(
                reference.origin,
                HirReferenceOrigin::Profile
                    | HirReferenceOrigin::Semantic
                    | HirReferenceOrigin::ScriptedMacro
                    | HirReferenceOrigin::DerivedLocalisation
            )
        })
        .filter(|reference| semantic_reference_is_active(&inactive_semantic_references, reference))
        .filter(|reference| scripted_macro_reference_is_callable(snapshot, hir, reference))
        .filter(|reference| {
            !matches!(
                reference.kind.to_ascii_lowercase().as_str(),
                "scripted_effect" | "scripted_trigger"
            ) || !reference
                .name
                .chars()
                .any(|character| character.is_whitespace() || matches!(character, '=' | '{' | '}'))
        })
        .filter(|reference| {
            reference.origin != HirReferenceOrigin::ScriptedMacro
                || workspace_member(snapshot, &reference.kind, &reference.name)
        })
    {
        cancellation.checkpoint()?;
        data.references.push(ReferenceInternal {
            kind: reference.kind.clone(),
            name: reference.name.clone(),
            range: reference.range,
            document: input.document.clone(),
            file: input.file,
            path: input.path.clone(),
        });
    }
    collect_quoted_semantics(snapshot, input, &mut data, cancellation)?;
    Ok(data)
}

fn collect_quoted_semantics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    data: &mut SemanticFile,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    if input.format != pdx_parser::FileFormat::Script {
        return Ok(());
    }
    let ParsedContent::Text(parsed) = &input.parsed;
    let mut quoted_scripts = QuotedScriptSession::new(cancellation);
    let mut collector = QuotedSemanticCollector {
        snapshot,
        input,
        data,
        quoted_scripts: &mut quoted_scripts,
    };
    for root in script_properties(input, parsed.root()) {
        collector.quoted_scripts.cancellation().checkpoint()?;
        let Some(context) = semantic_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let scope = semantic_initial_scope(snapshot, input, &context, &root.key, root.key_range);
        collector.collect(QuotedSemanticContainer {
            context: &context,
            parent_path: &[],
            scope: &scope,
            properties: &root.block,
            embedded: false,
            container_key: None,
            container_property: None,
            quoted_depth: 0,
        })?;
    }
    Ok(())
}

struct QuotedSemanticCollector<'snapshot, 'input, 'data, 'session, 'cancel> {
    snapshot: &'snapshot AnalysisSnapshot,
    input: &'input ParsedInput,
    data: &'data mut SemanticFile,
    quoted_scripts: &'session mut QuotedScriptSession<'cancel>,
}

struct QuotedSemanticContainer<'a> {
    context: &'a str,
    parent_path: &'a [String],
    scope: &'a ScopeContext,
    properties: &'a [ScriptProperty],
    embedded: bool,
    container_key: Option<&'a str>,
    container_property: Option<&'a ScriptProperty>,
    quoted_depth: usize,
}

impl QuotedSemanticCollector<'_, '_, '_, '_, '_> {
    fn collect(&mut self, container: QuotedSemanticContainer<'_>) -> Result<(), Cancelled> {
        for property in container.properties {
            self.quoted_scripts.cancellation().checkpoint()?;
            if container.embedded {
                collect_embedded_property_semantics(
                    EmbeddedSemanticInput {
                        snapshot: self.snapshot,
                        input: self.input,
                        data: self.data,
                    },
                    container.context,
                    container.parent_path,
                    container.scope,
                    container.container_key,
                    property,
                );
            }
            if property.block_range.is_none()
                && let Some(origin) = property.quoted_source.as_ref()
                && let Some(invocation) = container.container_property
            {
                let inference_context = SemanticCompletionContext {
                    context: container.context.to_owned(),
                    parent_path: container.parent_path.to_vec(),
                    structural_containers: Vec::new(),
                    alternative_containers: Vec::new(),
                    existing_keys: Vec::new(),
                    macro_inferred: false,
                    scope: container.scope.clone(),
                    container_property: Some(invocation.clone()),
                    property: Some(property.clone()),
                    quoted_depth: container.quoted_depth,
                    embedded_value_context: None,
                    wrapper_container: false,
                    root_entry_container: false,
                };
                let inferred = infer_macro_quoted_script_constraints(
                    self.snapshot,
                    &inference_context,
                    property,
                    self.quoted_scripts.cancellation(),
                )?;
                if !inferred.is_empty() {
                    if let QuotedScriptParse::Parsed(script) = self
                        .quoted_scripts
                        .parse(origin.source(), container.quoted_depth)?
                    {
                        let (quoted_properties, _) = quoted_script_container(&script, origin);
                        for site in inferred {
                            self.collect(QuotedSemanticContainer {
                                context: &site.context,
                                parent_path: &site.parent_path,
                                scope: &site.scope,
                                properties: &quoted_properties,
                                embedded: true,
                                container_key: None,
                                container_property: None,
                                quoted_depth: container.quoted_depth.saturating_add(1),
                            })?;
                        }
                    }
                    continue;
                }
            }
            let transparent = container.context.eq_ignore_ascii_case("trigger")
                && self
                    .snapshot
                    .game_profile()
                    .is_transparent_scope_wrapper(&property.key);
            let matching = semantic_rules_for_container_key(
                self.snapshot,
                container.context,
                container.parent_path,
                &property.key,
            )
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(
                        self.snapshot,
                        rule,
                        container.parent_path,
                        &property.key,
                    )
            })
            .collect::<Vec<_>>();
            let Some(selected) = semantic_selected_transition(SemanticTransitionInput {
                snapshot: self.snapshot,
                matching: &matching,
                selected_alternative: None,
                context: container.context,
                parent_path: container.parent_path,
                property,
                scope: container.scope,
                transparent_wrapper: transparent,
            }) else {
                continue;
            };
            let (next_context, next_path) = semantic_transition_destination(
                selected,
                container.context,
                container.parent_path,
                &property.key,
                transparent,
            );
            let next_scope = semantic_child_scope(self.snapshot, container.scope, selected);
            if matches!(selected.shape, pdx_rules::RuleShape::QuotedScript) {
                let Some(origin) = property.quoted_source.as_ref() else {
                    continue;
                };
                let QuotedScriptParse::Parsed(script) = self
                    .quoted_scripts
                    .parse(origin.source(), container.quoted_depth)?
                else {
                    continue;
                };
                let (quoted_properties, _) = quoted_script_container(&script, origin);
                self.collect(QuotedSemanticContainer {
                    context: &next_context,
                    parent_path: &next_path,
                    scope: &next_scope,
                    properties: &quoted_properties,
                    embedded: true,
                    container_key: None,
                    container_property: None,
                    quoted_depth: container.quoted_depth.saturating_add(1),
                })?;
            } else if property.block_range.is_some() {
                self.collect(QuotedSemanticContainer {
                    context: &next_context,
                    parent_path: &next_path,
                    scope: &next_scope,
                    properties: &property.block,
                    embedded: container.embedded,
                    container_key: Some(&property.key),
                    container_property: Some(property),
                    quoted_depth: container.quoted_depth,
                })?;
            }
        }
        Ok(())
    }
}

struct EmbeddedSemanticInput<'a> {
    snapshot: &'a AnalysisSnapshot,
    input: &'a ParsedInput,
    data: &'a mut SemanticFile,
}

fn collect_embedded_property_semantics(
    source: EmbeddedSemanticInput<'_>,
    context: &str,
    parent_path: &[String],
    scope: &ScopeContext,
    container_key: Option<&str>,
    property: &ScriptProperty,
) {
    let EmbeddedSemanticInput {
        snapshot,
        input,
        data,
    } = source;
    if let Some((value, range)) = property.scalar.as_ref() {
        if let Some(kind) = input.profile.reference_kind(&property.key)
            && !value.is_empty()
            && !value.eq_ignore_ascii_case("yes")
            && !value.eq_ignore_ascii_case("no")
            && value.parse::<f64>().is_err()
        {
            data.references
                .push(embedded_reference(input, kind, value, *range));
        }
        if let Some(kind) = input
            .profile
            .value_definition_kind(&property.key, container_key)
            && !value.is_empty()
        {
            data.definitions.push(make_definition(
                input,
                kind,
                value.clone(),
                property.range,
                *range,
            ));
        }
        if !property.quoted
            && semantic_rules_for_container_key(snapshot, context, parent_path, &property.key)
                .iter()
                .any(|rule| {
                    matches!(rule.shape, RuleShape::Leaf)
                        && matches!(rule.value, pdx_rules::ValueMatcher::Localisation)
                        && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                        && semantic_scope_allows(rule, scope)
                        && semantic_property_matches(snapshot, rule, property, scope)
                })
        {
            data.references
                .push(embedded_reference(input, "localisation", value, *range));
        }
    }

    for rule in semantic_rules_for_container_key(snapshot, context, parent_path, &property.key)
        .into_iter()
        .filter(|rule| {
            semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                && semantic_scope_allows(rule, scope)
        })
    {
        let type_name = match &rule.key {
            KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
                if scripted_macro_type(snapshot, type_name) =>
            {
                type_name
            }
            _ => continue,
        };
        let Some(summary) = macro_definition_summary(snapshot, type_name, &property.key) else {
            continue;
        };
        if scripted_macro_invocation_shape_matches(
            snapshot,
            type_name,
            &summary,
            property.scalar.as_ref().map(|(value, _)| value.as_str()),
            property.block_range.is_some(),
        ) {
            data.references.push(embedded_reference(
                input,
                type_name,
                &property.key,
                property.key_range,
            ));
            break;
        }
    }
}

fn embedded_reference(
    input: &ParsedInput,
    kind: &str,
    name: &str,
    range: TextRange,
) -> ReferenceInternal {
    ReferenceInternal {
        kind: kind.to_owned(),
        name: name.to_owned(),
        range,
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
    }
}

fn scripted_macro_reference_is_callable(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    reference: &HirReference,
) -> bool {
    if reference.origin != HirReferenceOrigin::ScriptedMacro {
        return true;
    }
    scripted_macro_reference_range_is_callable(
        snapshot,
        hir,
        &reference.kind,
        &reference.name,
        reference.range,
    )
}

fn scripted_macro_reference_range_is_callable(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    kind: &str,
    name: &str,
    range: TextRange,
) -> bool {
    let Some(summary) = macro_definition_summary(snapshot, kind, name) else {
        return false;
    };
    let Some(property) = hir
        .properties()
        .iter()
        .find(|property| property.key_range == range)
    else {
        return false;
    };
    scripted_macro_invocation_shape_matches(
        snapshot,
        kind,
        &summary,
        property.scalar.as_ref().map(|scalar| scalar.value.as_str()),
        property.scalar.is_none() && property.value_range.is_some(),
    )
}

pub(crate) fn inactive_semantic_reference_ranges(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
) -> BTreeSet<TextRange> {
    let mut inactive = BTreeSet::new();
    let mut invalid_ancestors = Vec::<(Vec<String>, TextRange)>::new();
    // Container rule sets are identical for every property in one (context, parent_path). Cache
    // them per context and path so dynamic members (e.g. one container per mission) do not
    // rebuild and re-filter the container rules for every property.
    let mut cached_containers =
        HashMap::<String, HashMap<Vec<String>, ContainerRuleCache<'_>>>::new();
    for property in hir.properties() {
        while invalid_ancestors.last().is_some_and(|(path, range)| {
            !property.path.starts_with(path) || !text_range_within(property.range, *range)
        }) {
            invalid_ancestors.pop();
        }
        let own_invalid =
            semantic_type_property_is_invalid(snapshot, hir, property, &mut cached_containers);
        if (!invalid_ancestors.is_empty() || own_invalid)
            && let Some(scalar) = property.scalar.as_ref()
        {
            inactive.insert(scalar.range);
        }
        if own_invalid {
            invalid_ancestors.push((property.path.clone(), property.range));
        }
    }
    inactive
}

/// One (context, parent_path) container's rule set, with derived fast-path indexes so the
/// per-property validity check does not rescan the container rules for every property.
pub(crate) struct ContainerRuleCache<'a> {
    pub(crate) rules: Vec<&'a pdx_rules::SemanticRule>,
    /// Lowercased keys of non-leaf exact rules; a property key in this set is valid by a concrete
    /// match without scanning `rules`.
    pub(crate) concrete_keys: HashSet<String>,
    /// Whether any concrete non-leaf rule uses an `AnyScalar` matcher (matches every key).
    pub(crate) any_scalar_concrete: bool,
    /// Whether the container carries concrete non-leaf rules at all (enum/qualified matchers that
    /// the key set cannot express still need the scan).
    pub(crate) has_concrete: bool,
    /// Whether the container carries `Type` matchers, the only rules the workspace check applies
    /// to.
    pub(crate) has_type: bool,
}

pub(crate) fn semantic_type_property_is_invalid<'a>(
    snapshot: &'a AnalysisSnapshot,
    hir: &HirFile,
    property: &pdx_engine::hir::HirProperty,
    cached_containers: &mut HashMap<String, HashMap<Vec<String>, ContainerRuleCache<'a>>>,
) -> bool {
    if property.path.len() <= 1 {
        return false;
    }
    let Some(fact) = hir.scope_fact_at(property.key_range) else {
        return false;
    };
    let by_path = match cached_containers.get_mut(fact.context.as_str()) {
        Some(by_path) => by_path,
        None => cached_containers.entry(fact.context.clone()).or_default(),
    };
    if !by_path.contains_key(fact.parent_path.as_slice()) {
        // `semantic_rules_for_container` ignores its scope argument; build it once per container
        // only so the caller does not allocate a scope context for every property.
        let scope = scope_context_from_hir(snapshot.game_profile_handle(), &fact.state);
        let rules =
            semantic_rules_for_container(snapshot, &fact.context, &fact.parent_path, &scope);
        let mut concrete_keys = HashSet::new();
        let mut any_scalar_concrete = false;
        let mut has_concrete = false;
        let mut has_type = false;
        for rule in &rules {
            match &rule.key {
                KeyMatcher::Type(_) => has_type = true,
                KeyMatcher::Dynamic(_) => {}
                KeyMatcher::Exact(key) if !matches!(rule.shape, RuleShape::LeafValue) => {
                    has_concrete = true;
                    concrete_keys.insert(key.to_ascii_lowercase());
                }
                KeyMatcher::AnyScalar if !matches!(rule.shape, RuleShape::LeafValue) => {
                    has_concrete = true;
                    any_scalar_concrete = true;
                }
                KeyMatcher::Exact(_)
                | KeyMatcher::AnyScalar
                | KeyMatcher::Date
                | KeyMatcher::Enum(_) => {}
            }
        }
        by_path.insert(
            fact.parent_path.clone(),
            ContainerRuleCache {
                rules,
                concrete_keys,
                any_scalar_concrete,
                has_concrete,
                has_type,
            },
        );
    }
    let entry = by_path
        .get(fact.parent_path.as_slice())
        .expect("filled above");
    if entry.any_scalar_concrete
        || entry
            .concrete_keys
            .contains(&property.key.to_ascii_lowercase())
    {
        return false;
    }
    if entry.has_concrete
        && entry.rules.iter().any(|rule| {
            !matches!(rule.key, KeyMatcher::Type(_) | KeyMatcher::Dynamic(_))
                && !matches!(rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(snapshot, rule, &fact.parent_path, &property.key)
        })
    {
        return false;
    }
    // The workspace check below only fires for containers that actually carry Type matchers.
    if !entry.has_type {
        return false;
    }
    entry.rules.iter().any(|rule| {
        let KeyMatcher::Type(type_name) = &rule.key else {
            return false;
        };
        match workspace_type_member(snapshot, type_name, &property.key) {
            WorkspaceTypeMember::Present => false,
            WorkspaceTypeMember::Absent => true,
            WorkspaceTypeMember::Unknown => {
                !type_member_provably_valid(snapshot, type_name, &property.key)
            }
        }
    })
}

pub(crate) fn semantic_reference_is_active(
    inactive_semantic_references: &BTreeSet<TextRange>,
    reference: &HirReference,
) -> bool {
    if reference.origin != HirReferenceOrigin::Semantic {
        return true;
    }
    !inactive_semantic_references.contains(&reference.range)
}

pub(crate) fn text_range_within(inner: TextRange, outer: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}
pub(crate) fn make_definition(
    input: &ParsedInput,
    kind: &str,
    name: String,
    range: TextRange,
    selection_range: TextRange,
) -> DefinitionInfo {
    let location = Location {
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        range,
    };
    DefinitionInfo {
        kind: kind.to_owned(),
        name: name.clone(),
        symbol: Symbol {
            name,
            kind: kind.to_owned(),
            range,
            selection_range,
            location,
        },
        document: input.document.clone(),
        file: input.file,
    }
}
pub(crate) fn all_semantics(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<SemanticWorkspace, Cancelled> {
    #[cfg(test)]
    ALL_SEMANTICS_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    let mut all = SemanticWorkspace::default();
    // Indexed definitions and references remain in `AnalysisSnapshot::index()` and are consulted
    // by targeted candidate/reference iterators below. Keeping them out of this temporary
    // semantic workspace avoids cloning every cached Vanilla symbol for each query.
    let overlay_files = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect::<BTreeSet<_>>();
    for file in snapshot.source_files().values() {
        cancellation.checkpoint()?;
        if overlay_files.contains(&file.id) {
            continue;
        }
        let Some(input) = input_for_source_file(snapshot, file.id) else {
            continue;
        };
        let mut quoted = SemanticFile {
            definitions: Vec::new(),
            references: Vec::new(),
        };
        collect_quoted_semantics(snapshot, &input, &mut quoted, cancellation)?;
        all.definitions.extend(quoted.definitions);
        all.references.extend(quoted.references);
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            let semantic = semantic_data_with_cancellation(snapshot, &input, cancellation)?;
            all.definitions.extend(semantic.definitions);
            all.references.extend(semantic.references);
        }
    }
    Ok(all)
}

#[cfg(test)]
thread_local! {
    pub(crate) static ALL_SEMANTICS_CALLS: Cell<usize> = const { Cell::new(0) };
}
pub(crate) fn resolve_symbol(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Resolution {
    let mut candidates = symbol_candidates(snapshot, all, kind, name);
    if candidates.is_empty() {
        return Resolution::Missing;
    }
    let policy = symbol_resolution_policy(snapshot, kind);
    if matches!(
        policy,
        SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique
    ) {
        return if candidates.len() == 1 {
            Resolution::Unique(candidates.remove(0))
        } else {
            Resolution::Ambiguous
        };
    }
    let highest = candidates
        .iter()
        .map(|candidate| candidate.priority)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.priority == highest);
    if candidates.len() == 1 {
        Resolution::Unique(candidates.remove(0))
    } else {
        Resolution::Ambiguous
    }
}

pub(crate) fn symbol_candidates(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Vec<ResolutionDefinition> {
    let overlay_files = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect::<BTreeSet<_>>();
    let mut candidates = all
        .definitions
        .iter()
        .filter(|definition| definition.kind == kind && same_name(&definition.name, name))
        .filter(|definition| {
            definition
                .file
                .is_none_or(|file| !overlay_files.contains(&file) || definition.document.is_some())
        })
        .map(|definition| ResolutionDefinition {
            location: definition.symbol.location.clone(),
            selection_range: definition.symbol.selection_range,
            priority: definition_priority(snapshot, definition),
        })
        .collect::<Vec<_>>();
    // Indexed definitions are the normal source of Vanilla/dependency candidates. Add them even
    // when an overlay supplied a same-named semantic definition: merge/unique policies need to
    // see every candidate, while overlay files hide their corresponding disk entries.
    for definition in snapshot.index().definitions(kind, name) {
        if overlay_files.contains(&definition.file_id) {
            continue;
        }
        candidates.push(index_definition(snapshot, definition));
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    if kind.eq_ignore_ascii_case("localisation") {
        candidates = prefer_localisation_language(candidates);
    }
    candidates
}

pub(crate) fn symbol_candidates_for_hover(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<ResolutionDefinition>, Cancelled> {
    let overlay_files = overlay_file_ids(snapshot);
    let mut candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        cancellation.checkpoint()?;
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        for definition in semantic_data(snapshot, &input).definitions {
            cancellation.checkpoint()?;
            if definition.kind != kind || !same_name(&definition.name, name) {
                continue;
            }
            let priority = definition_priority(snapshot, &definition);
            candidates.push(ResolutionDefinition {
                location: definition.symbol.location,
                selection_range: definition.symbol.selection_range,
                priority,
            });
        }
    }
    for definition in snapshot.index().definitions(kind, name) {
        cancellation.checkpoint()?;
        if overlay_files.contains(&definition.file_id) {
            continue;
        }
        candidates.push(index_definition(snapshot, definition));
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    if kind.eq_ignore_ascii_case("localisation") {
        candidates = prefer_localisation_language(candidates);
    }
    Ok(candidates)
}

pub(crate) fn symbol_resolution_policy(
    snapshot: &AnalysisSnapshot,
    kind: &str,
) -> SymbolResolutionPolicy {
    snapshot
        .rules()
        .model()
        .symbol_descriptors
        .iter()
        .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(kind))
        .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
            descriptor.resolution
        })
}

pub(crate) fn symbol_location_sort_key(location: &Location) -> (String, u32, u32) {
    (
        location
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.as_str().to_owned()),
        location.range.start(),
        location.range.end(),
    )
}

/// Extracts the localisation language from a file's logical path. Both `localisation/l_english/...`
/// and `localisation/domination_l_english.yml` yield `english`; non-localisation files yield `None`.
pub(crate) fn localisation_language(path: Option<&LogicalPath>) -> Option<String> {
    let path = path?.as_str();
    for segment in path.split('/') {
        if let Some(language) = segment
            .strip_prefix("l_")
            .filter(|language| !language.is_empty())
        {
            return Some(language.to_owned());
        }
    }
    let file = path.rsplit('/').next()?;
    let after = file
        .rfind("_l_")
        .map(|index| &file[index + 3..])
        .unwrap_or_default();
    let language = after
        .strip_suffix(".yml")
        .or_else(|| after.strip_suffix(".yaml"))
        .unwrap_or(after);
    (!language.is_empty()).then(|| language.to_owned())
}

/// Vanilla localisation defines every key once per supported language. When a localisation symbol
/// has candidates in several languages, prefer the English definition so the per-language variants
/// do not look ambiguous; when no English definition exists, keep all candidates unchanged so
/// single-language mods still resolve.
pub(crate) fn prefer_localisation_language(
    candidates: Vec<ResolutionDefinition>,
) -> Vec<ResolutionDefinition> {
    if candidates.len() < 2 {
        return candidates;
    }
    let english = candidates
        .iter()
        .filter(|candidate| {
            localisation_language(candidate.location.path.as_ref()).as_deref() == Some("english")
        })
        .cloned()
        .collect::<Vec<_>>();
    if english.is_empty() {
        candidates
    } else {
        english
    }
}

/// Resolves localisation keys to their displayed values in a single workspace
/// pass, mirroring symbol resolution semantics: per-language variants prefer
/// the English definition, and only keys with a non-empty active definition
/// are returned. Keys without a definition are simply absent from the map.
///
/// Used by the LSP mission preview to resolve mission titles (`{id}_title`).
/// Each key is resolved with a targeted overlay scan plus an exact index lookup
/// (mirroring `symbol_candidates_for_hover`), so a request never rebuilds the
/// full workspace semantics — resolving a handful of titles against an
/// EU4-scale index stays cheap.
pub fn localisation_values_by_key<'a>(
    snapshot: &AnalysisSnapshot,
    keys: &'a [&'a str],
    cancellation: &CancellationToken,
) -> Result<HashMap<String, (Option<String>, String)>, Cancelled> {
    let mut resolved = HashMap::new();
    for &key in keys {
        cancellation.checkpoint()?;
        let Some(definition) =
            symbol_candidates_for_hover(snapshot, "localisation", key, cancellation)?
                .into_iter()
                .next()
        else {
            continue;
        };
        if let Some(preview) = crate::hover::localisation_preview(snapshot, &definition) {
            resolved.insert(key.to_owned(), preview);
        }
    }
    Ok(resolved)
}

pub(crate) struct DirectResolutionContext<'snapshot> {
    pub(crate) snapshot: &'snapshot AnalysisSnapshot,
    pub(crate) overlay_files: BTreeSet<SourceFileId>,
    pub(crate) overlay_definitions: BTreeMap<(String, String), Vec<ResolutionDefinition>>,
}

impl<'snapshot> DirectResolutionContext<'snapshot> {
    pub(crate) fn new(snapshot: &'snapshot AnalysisSnapshot) -> Self {
        let mut context = Self {
            snapshot,
            overlay_files: BTreeSet::new(),
            overlay_definitions: BTreeMap::new(),
        };
        for document in snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
        {
            if let Some(path) = document.path()
                && let Some(file) = snapshot.source_file_id_for_path(path)
            {
                context.overlay_files.insert(file);
            }
            let Some(input) = input_for_document(snapshot, document.id()) else {
                continue;
            };
            for definition in semantic_data(snapshot, &input).definitions {
                let priority = definition_priority(snapshot, &definition);
                context
                    .overlay_definitions
                    .entry((
                        definition.kind.to_ascii_lowercase(),
                        definition.name.to_ascii_lowercase(),
                    ))
                    .or_default()
                    .push(ResolutionDefinition {
                        location: definition.symbol.location,
                        selection_range: definition.symbol.selection_range,
                        priority,
                    });
            }
        }
        context
    }

    pub(crate) fn resolve(&self, kind: &str, name: &str) -> Resolution {
        let mut candidates = self
            .overlay_definitions
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default();
        candidates.extend(
            self.snapshot
                .index()
                .definitions(kind, name)
                .into_iter()
                .filter(|definition| !self.overlay_files.contains(&definition.file_id))
                .map(|definition| index_definition(self.snapshot, definition)),
        );
        if kind.eq_ignore_ascii_case("localisation") {
            candidates = prefer_localisation_language(candidates);
        }
        if candidates.is_empty() {
            return Resolution::Missing;
        }
        let policy = self
            .snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(kind))
            .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
                descriptor.resolution
            });
        if matches!(
            policy,
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique
        ) {
            return if candidates.len() == 1 {
                Resolution::Unique(candidates.remove(0))
            } else {
                Resolution::Ambiguous
            };
        }
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        candidates.retain(|candidate| candidate.priority == highest);
        if candidates.len() == 1 {
            Resolution::Unique(candidates.remove(0))
        } else {
            Resolution::Ambiguous
        }
    }
}

pub(crate) fn definition_priority(snapshot: &AnalysisSnapshot, definition: &DefinitionInfo) -> u64 {
    if definition.document.is_some() {
        return 20_000;
    }
    let Some(file) = definition
        .file
        .and_then(|id| snapshot.source_files().get(&id))
    else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        pdx_engine::SourceRootKind::Vanilla => 0,
        pdx_engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_engine::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}

pub(crate) fn index_definition(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> ResolutionDefinition {
    let (path, document) = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| (Some(file.logical_path.clone()), None))
        .unwrap_or((None, None));
    ResolutionDefinition {
        location: Location {
            document,
            file: Some(definition.file_id),
            path,
            range: definition.range,
        },
        selection_range: indexed_definition_selection_range(snapshot, definition),
        priority: definition_priority_for_file(snapshot, definition.file_id),
    }
}

/// Converts one persisted reference into an editor-neutral location, retaining the same macro
/// callability filters used when exhaustive semantic workspaces were built.
pub(crate) fn indexed_reference(
    snapshot: &AnalysisSnapshot,
    reference: &Reference,
) -> Option<ReferenceInternal> {
    if scripted_macro_type(snapshot, &reference.kind) {
        if !workspace_member(snapshot, &reference.kind, &reference.name) {
            return None;
        }
        if let Some(hir) = snapshot
            .file_state(reference.file_id)
            .and_then(|state| state.hir())
            && !scripted_macro_reference_range_is_callable(
                snapshot,
                hir,
                &reference.kind,
                &reference.name,
                reference.range,
            )
        {
            return None;
        }
    }
    let path = snapshot
        .source_files()
        .get(&reference.file_id)
        .map(|file| file.logical_path.clone());
    Some(ReferenceInternal {
        kind: reference.kind.clone(),
        name: reference.name.clone(),
        range: reference.range,
        document: None,
        file: Some(reference.file_id),
        path,
    })
}

pub(crate) fn index_definition_info(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> DefinitionInfo {
    let selection_range = indexed_definition_selection_range(snapshot, definition);
    let path = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| file.logical_path.clone());
    let location = Location {
        document: None,
        file: Some(definition.file_id),
        path,
        range: definition.range,
    };
    DefinitionInfo {
        kind: definition.kind.clone(),
        name: definition.name.clone(),
        symbol: Symbol {
            name: definition.name.clone(),
            kind: definition.kind.clone(),
            range: definition.range,
            selection_range,
            location,
        },
        document: None,
        file: Some(definition.file_id),
    }
}

pub(crate) fn indexed_definition_selection_range(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> TextRange {
    snapshot
        .file_state(definition.file_id)
        .and_then(|state| state.hir())
        .and_then(|hir| {
            hir.definitions()
                .iter()
                .find(|candidate| {
                    candidate.kind.eq_ignore_ascii_case(&definition.kind)
                        && candidate.name.eq_ignore_ascii_case(&definition.name)
                        && candidate.range == definition.range
                })
                .map(|candidate| candidate.selection_range)
        })
        .unwrap_or(definition.range)
}

pub(crate) fn definition_selection_location(definition: &ResolutionDefinition) -> Location {
    let mut location = definition.location.clone();
    location.range = definition.selection_range;
    location
}

pub(crate) fn definition_priority_for_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> u64 {
    let Some(file) = snapshot.source_files().get(&id) else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        pdx_engine::SourceRootKind::Vanilla => 0,
        pdx_engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        pdx_engine::SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}
pub(crate) fn symbol_at(
    all: &SemanticWorkspace,
    document: &DocumentId,
    position: TextSize,
) -> Option<(String, String)> {
    if let Some(reference) = all.references.iter().find(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    }) {
        return Some((reference.kind.clone(), reference.name.clone()));
    }
    all.definitions
        .iter()
        .find(|definition| {
            definition.document.as_ref() == Some(document)
                && contains(definition.symbol.selection_range, position)
        })
        .map(|definition| (definition.kind.clone(), definition.name.clone()))
}

pub(crate) fn local_parameter_target(
    input: &ParsedInput,
    position: TextSize,
) -> Option<(
    &pdx_engine::hir::HirParameterDefinition,
    &pdx_engine::hir::HirParameterReference,
)> {
    let hir = input.hir.as_deref()?;
    let reference = hir.parameter_reference_at(position)?;
    let definition = hir
        .parameter_definitions_for_owner(reference.owner_range)
        .find(|definition| definition.name.eq_ignore_ascii_case(&reference.name))?;
    Some((definition, reference))
}
