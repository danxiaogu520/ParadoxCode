//! Per-file shards and deterministic workspace symbol lookup.

use std::collections::{BTreeMap, BTreeSet};

use pdx_rules::{RuleSet, SymbolResolutionPolicy};
use pdx_text::{PositionRange, TextRange};

use crate::hir::MacroTemplate;
use crate::model::{SourceFileId, WorkspaceError, WorkspaceScanToken};

/// One parameter in the callable signature of a scripted macro definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacroParameterSignature {
    /// Parameter spelling as inferred from the definition body.
    pub name: String,
    /// Whether every observed use requires the caller to provide this parameter.
    pub required: bool,
}

/// Indexed callable metadata derived from one scripted effect or trigger definition.
///
/// The optional template is normalized, source-ranged semantic IR. It contains neither source
/// text nor CST pointers, so the same representation can drive live-workspace and cache-only
/// analysis without teaching the engine any concrete macro names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacroDefinitionSummary {
    /// Dynamic symbol kind, for example `scripted_effect`.
    pub kind: String,
    /// Definition name as written in source.
    pub name: String,
    /// Full range of the owning symbol definition.
    pub definition_range: TextRange,
    /// Parameters in first-use order.
    pub parameters: Vec<MacroParameterSignature>,
    /// Reusable body semantics, when the definition could be lowered as a macro template.
    pub template: Option<MacroTemplate>,
}

/// One symbol definition retained in an index shard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Definition {
    /// Semantic kind, for example event or localisation.
    pub kind: String,
    /// Symbol name as written in source.
    pub name: String,
    /// Defining file.
    pub file_id: SourceFileId,
    /// Source range of the definition.
    pub range: TextRange,
    /// Whether this definition wins symbol resolution.
    pub active: bool,
}

/// A source reference retained for later semantic resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    /// Reference category.
    pub kind: String,
    /// Referenced name.
    pub name: String,
    /// Referencing file.
    pub file_id: SourceFileId,
    /// Source range of the reference.
    pub range: TextRange,
}

/// Atomic parse/HIR/index output for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIndexShard {
    /// File that produced this shard.
    pub file_id: SourceFileId,
    /// Definitions in source order.
    pub definitions: Vec<Definition>,
    /// References in source order.
    pub references: Vec<Reference>,
    /// Callable signatures and normalized templates for scripted macro definitions in this file.
    pub macro_definitions: Vec<MacroDefinitionSummary>,
    /// Syntax error count retained as a cheap health signal.
    pub syntax_error_count: usize,
}

/// Workspace-wide symbol index made from immutable file shards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DefinitionPointer {
    file_id: SourceFileId,
    ordinal: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceIndex {
    pub(crate) shards: BTreeMap<SourceFileId, FileIndexShard>,
    definitions: BTreeMap<(String, String), Vec<DefinitionPointer>>,
    case_sensitive_kinds: BTreeSet<String>,
    /// Cached UTF-16 positions for files whose source text is not retained, such as Vanilla.
    pub(crate) position_ranges: BTreeMap<(SourceFileId, TextRange), PositionRange>,
}

impl WorkspaceIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Applies symbol-name case policies from the immutable rule set and rebuilds lookup maps.
    ///
    /// The index historically lower-cased every symbol name.  Keeping the policy on the index
    /// makes the lookup identity explicit while preserving the cheap bucketed queries used by
    /// analysis.
    pub fn configure_case_sensitivity(&mut self, rules: &RuleSet) {
        self.case_sensitive_kinds = rules
            .model()
            .symbol_descriptors
            .iter()
            .filter(|descriptor| descriptor.case_sensitive)
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase())
            .collect();
        self.rebuild_maps();
    }

    /// Builds an index from a complete set of file shards and derives lookup maps once.
    #[must_use]
    pub fn from_shards(shards: impl IntoIterator<Item = FileIndexShard>) -> Self {
        match Self::from_shards_cancellable(shards, &WorkspaceScanToken::new()) {
            Ok(index) => index,
            Err(WorkspaceError::Cancelled) => {
                unreachable!("a fresh workspace scan token cannot be cancelled")
            }
            Err(_) => unreachable!("index construction has no other fallible operation"),
        }
    }

    fn from_shards_cancellable(
        shards: impl IntoIterator<Item = FileIndexShard>,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, WorkspaceError> {
        let mut index = Self::empty();
        for shard in shards {
            cancellation.checkpoint()?;
            index.shards.insert(shard.file_id, shard);
        }
        index.rebuild_maps_cancellable(cancellation)?;
        Ok(index)
    }

    pub(crate) fn from_shards_cancellable_with_rules(
        shards: impl IntoIterator<Item = FileIndexShard>,
        rules: &RuleSet,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, WorkspaceError> {
        let mut index = Self::empty();
        index.case_sensitive_kinds = rules
            .model()
            .symbol_descriptors
            .iter()
            .filter(|descriptor| descriptor.case_sensitive)
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase())
            .collect();
        for shard in shards {
            cancellation.checkpoint()?;
            index.shards.insert(shard.file_id, shard);
        }
        index.rebuild_maps_cancellable(cancellation)?;
        Ok(index)
    }

    /// Returns all retained definitions for a kind/name, including shadowed ones.
    #[must_use]
    pub fn definitions(&self, kind: &str, name: &str) -> Vec<&Definition> {
        self.definitions
            .get(&(kind.to_owned(), self.lookup_name(kind, name)))
            .into_iter()
            .flatten()
            .filter_map(|pointer| self.definition_at(*pointer))
            .collect()
    }

    /// Returns the active definition for a kind/name, if one exists.
    #[must_use]
    pub fn active_definition(&self, kind: &str, name: &str) -> Option<&Definition> {
        let definitions = self.definitions(kind, name);
        let mut active = definitions
            .into_iter()
            .filter(|definition| definition.active);
        let definition = active.next()?;
        active
            .all(|candidate| {
                candidate.file_id == definition.file_id && candidate.range == definition.range
            })
            .then_some(definition)
    }

    /// Iterates over all retained definitions in deterministic kind/name order.
    #[must_use = "iterate the retained definitions"]
    pub fn definitions_iter(&self) -> impl Iterator<Item = &Definition> {
        self.definitions
            .values()
            .flatten()
            .filter_map(|pointer| self.definition_at(*pointer))
    }

    /// Iterates over retained definitions of one exact kind without scanning unrelated symbols.
    #[must_use = "iterate the retained definitions"]
    pub fn definitions_for_kind<'index>(
        &'index self,
        kind: &str,
    ) -> impl Iterator<Item = &'index Definition> {
        let first_key = (kind.to_owned(), String::new());
        let expected_kind = kind.to_owned();
        self.definitions
            .range(first_key..)
            .take_while(move |((candidate_kind, _), _)| candidate_kind == &expected_kind)
            .flat_map(|(_, pointers)| pointers)
            .filter_map(|pointer| self.definition_at(*pointer))
    }

    /// Returns the shard for a file.
    #[must_use]
    pub fn shard(&self, file_id: SourceFileId) -> Option<&FileIndexShard> {
        self.shards.get(&file_id)
    }

    /// Returns all references from a file.
    #[must_use]
    pub fn references(&self, file_id: SourceFileId) -> &[Reference] {
        self.shards
            .get(&file_id)
            .map_or(&[], |shard| shard.references.as_slice())
    }

    /// Returns the callable summary belonging to the uniquely active macro definition.
    #[must_use]
    pub fn active_macro_definition(
        &self,
        kind: &str,
        name: &str,
    ) -> Option<&MacroDefinitionSummary> {
        let definition = self.active_definition(kind, name)?;
        self.shards
            .get(&definition.file_id)?
            .macro_definitions
            .iter()
            .find(|summary| {
                summary.definition_range == definition.range
                    && summary.kind.eq_ignore_ascii_case(kind)
                    && summary.name.eq_ignore_ascii_case(name)
            })
    }

    /// Iterates over references from every retained file shard.
    #[must_use = "iterate the retained references"]
    pub fn references_iter(&self) -> impl Iterator<Item = &Reference> {
        self.shards
            .values()
            .flat_map(|shard| shard.references.iter())
    }

    /// Returns a cached editor position for one indexed byte range, if available.
    #[must_use]
    pub fn position_for(&self, file_id: SourceFileId, range: TextRange) -> Option<PositionRange> {
        self.position_ranges.get(&(file_id, range)).copied()
    }

    /// Returns all cached editor positions retained by this index.
    #[must_use]
    pub fn position_ranges(&self) -> &BTreeMap<(SourceFileId, TextRange), PositionRange> {
        &self.position_ranges
    }

    /// Replaces cached editor positions for one source file.
    pub fn replace_position_ranges(
        &mut self,
        file_id: SourceFileId,
        positions: impl IntoIterator<Item = (TextRange, PositionRange)>,
    ) {
        self.position_ranges
            .retain(|(candidate, _), _| *candidate != file_id);
        self.position_ranges.extend(
            positions
                .into_iter()
                .map(|(range, position)| ((file_id, range), position)),
        );
    }

    /// Replaces all cached editor positions.
    pub fn replace_all_position_ranges(
        &mut self,
        positions: BTreeMap<(SourceFileId, TextRange), PositionRange>,
    ) {
        self.position_ranges = positions;
    }

    /// Removes cached editor positions for one source file.
    pub fn remove_position_ranges(&mut self, file_id: SourceFileId) {
        self.position_ranges
            .retain(|(candidate, _), _| *candidate != file_id);
    }

    fn definition_at(&self, pointer: DefinitionPointer) -> Option<&Definition> {
        self.shards
            .get(&pointer.file_id)?
            .definitions
            .get(pointer.ordinal)
    }

    /// Replaces one shard and updates only lookup buckets touched by that file.
    pub fn replace_shard(&mut self, shard: FileIndexShard) {
        self.replace_shard_entries(shard);
    }

    /// Removes a file shard and updates only lookup buckets touched by that file.
    pub fn remove_shard(&mut self, file_id: SourceFileId) {
        let affected = self.remove_shard_entries(file_id);
        self.sort_definition_buckets(&affected);
    }

    pub(crate) fn remove_shard_resolved(
        &mut self,
        file_id: SourceFileId,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        let affected = self.remove_shard_entries(file_id);
        self.resolve_definition_buckets(&affected, priorities, rules);
    }

    pub(crate) fn resolve_priorities(
        &mut self,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        match self.resolve_priorities_cancellable(priorities, rules, &WorkspaceScanToken::new()) {
            Ok(()) => {}
            Err(WorkspaceError::Cancelled) => {
                unreachable!("a fresh workspace scan token cannot be cancelled")
            }
            Err(_) => unreachable!("priority resolution has no other fallible operation"),
        }
    }

    pub(crate) fn resolve_priorities_cancellable(
        &mut self,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
        cancellation: &WorkspaceScanToken,
    ) -> Result<(), WorkspaceError> {
        let keys = self.definitions.keys().cloned().collect::<Vec<_>>();
        for key in &keys {
            cancellation.checkpoint()?;
            self.resolve_definition_buckets(std::slice::from_ref(key), priorities, rules);
        }
        Ok(())
    }

    pub(crate) fn replace_shard_resolved(
        &mut self,
        shard: FileIndexShard,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        let affected = self.replace_shard_entries(shard);
        self.resolve_definition_buckets(&affected, priorities, rules);
    }

    fn replace_shard_entries(&mut self, shard: FileIndexShard) -> Vec<(String, String)> {
        let file_id = shard.file_id;
        let mut affected = self.remove_shard_entries(file_id);
        for (ordinal, definition) in shard.definitions.iter().enumerate() {
            let key = self.definition_key(definition);
            self.definitions
                .entry(key.clone())
                .or_default()
                .push(DefinitionPointer { file_id, ordinal });
            affected.push(key);
        }
        self.shards.insert(file_id, shard);
        affected.sort();
        affected.dedup();
        self.sort_definition_buckets(&affected);
        affected
    }

    fn remove_shard_entries(&mut self, file_id: SourceFileId) -> Vec<(String, String)> {
        self.remove_position_ranges(file_id);
        let Some(previous) = self.shards.remove(&file_id) else {
            return Vec::new();
        };
        let mut affected = previous
            .definitions
            .iter()
            .map(|definition| self.definition_key(definition))
            .collect::<Vec<_>>();
        affected.sort();
        affected.dedup();
        for key in &affected {
            let remove_bucket = self.definitions.get_mut(key).is_some_and(|definitions| {
                definitions.retain(|definition| definition.file_id != file_id);
                definitions.is_empty()
            });
            if remove_bucket {
                self.definitions.remove(key);
            }
        }
        affected
    }

    fn resolve_definition_buckets(
        &mut self,
        keys: &[(String, String)],
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        for key in keys {
            let Some(values) = self.definitions.get(key).cloned() else {
                continue;
            };
            let policy = rules
                .model()
                .symbol_descriptors
                .iter()
                .find(|descriptor| descriptor.kind_id.eq_ignore_ascii_case(&key.0))
                .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
                    descriptor.resolution
                });
            let highest = values
                .iter()
                .map(|pointer| priorities.get(&pointer.file_id).copied().unwrap_or(0))
                .max();
            for pointer in values {
                let Some(definition) = self
                    .shards
                    .get_mut(&pointer.file_id)
                    .and_then(|shard| shard.definitions.get_mut(pointer.ordinal))
                else {
                    continue;
                };
                definition.active = match policy {
                    SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
                    SymbolResolutionPolicy::ReplaceBySymbol => {
                        Some(priorities.get(&definition.file_id).copied().unwrap_or(0)) == highest
                    }
                };
            }
            self.sort_definition_buckets(std::slice::from_ref(key));
        }
    }

    fn sort_definition_buckets(&mut self, keys: &[(String, String)]) {
        let shards = &self.shards;
        for key in keys {
            if let Some(values) = self.definitions.get_mut(key) {
                values.sort_by_key(|pointer| {
                    shards
                        .get(&pointer.file_id)
                        .and_then(|shard| shard.definitions.get(pointer.ordinal))
                        .map_or((true, pointer.file_id), |definition| {
                            (!definition.active, definition.file_id)
                        })
                });
            }
        }
    }

    fn rebuild_maps(&mut self) {
        match self.rebuild_maps_cancellable(&WorkspaceScanToken::new()) {
            Ok(()) => {}
            Err(WorkspaceError::Cancelled) => unreachable!("a fresh index rebuild cannot cancel"),
            Err(_) => unreachable!("index rebuild has no other fallible operation"),
        }
    }

    fn rebuild_maps_cancellable(
        &mut self,
        cancellation: &WorkspaceScanToken,
    ) -> Result<(), WorkspaceError> {
        self.definitions.clear();
        for (file_id, shard) in &self.shards {
            cancellation.checkpoint()?;
            for (ordinal, definition) in shard.definitions.iter().enumerate() {
                cancellation.checkpoint()?;
                self.definitions
                    .entry((
                        definition.kind.clone(),
                        self.lookup_name(&definition.kind, &definition.name),
                    ))
                    .or_default()
                    .push(DefinitionPointer {
                        file_id: *file_id,
                        ordinal,
                    });
            }
        }
        let shards = &self.shards;
        for values in self.definitions.values_mut() {
            cancellation.checkpoint()?;
            values.sort_by_key(|pointer| {
                shards
                    .get(&pointer.file_id)
                    .and_then(|shard| shard.definitions.get(pointer.ordinal))
                    .map_or((true, pointer.file_id), |definition| {
                        (!definition.active, definition.file_id)
                    })
            });
        }
        Ok(())
    }
}

impl WorkspaceIndex {
    fn is_case_sensitive(&self, kind: &str) -> bool {
        self.case_sensitive_kinds
            .contains(&kind.to_ascii_lowercase())
    }

    fn lookup_name(&self, kind: &str, name: &str) -> String {
        if self.is_case_sensitive(kind) {
            name.to_owned()
        } else {
            name.to_ascii_lowercase()
        }
    }

    fn definition_key(&self, definition: &Definition) -> (String, String) {
        (
            definition.kind.clone(),
            self.lookup_name(&definition.kind, &definition.name),
        )
    }
}
