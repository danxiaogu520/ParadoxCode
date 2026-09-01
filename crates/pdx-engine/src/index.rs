//! Per-file shards and deterministic workspace symbol lookup.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pdx_rules::{RuleSet, SymbolResolutionPolicy};
use pdx_text::{PositionRange, TextRange};

use crate::hir::MacroTemplate;
use crate::model::{LocalisationPreview, SourceFileId, WorkspaceError, WorkspaceScanToken};

/// Compact, file-grouped UTF-16 positions for indexed ranges.
///
/// A flat `BTreeMap<(SourceFileId, TextRange), PositionRange>` needs one tree node for every
/// definition and reference.  EU4's Vanilla index contains millions of those entries, so the
/// node allocator overhead is larger than the position payload itself.  Grouping by file keeps
/// the same deterministic lookup semantics while storing only one vector allocation per file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PositionMap {
    by_file: BTreeMap<SourceFileId, Vec<(TextRange, PositionRange)>>,
    len: usize,
}

impl PositionMap {
    /// Creates an empty position map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of indexed ranges.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no indexed ranges are retained.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up one file/range pair.
    #[must_use]
    pub fn get<K>(&self, key: K) -> Option<&PositionRange>
    where
        K: std::borrow::Borrow<(SourceFileId, TextRange)>,
    {
        let key = key.borrow();
        let entries = self.by_file.get(&key.0)?;
        let index = entries
            .binary_search_by_key(&key.1, |(range, _)| *range)
            .ok()?;
        entries.get(index).map(|(_, position)| position)
    }

    /// Iterates in stable `(file_id, range)` order.
    pub fn iter(&self) -> PositionMapIter<'_> {
        PositionMapIter {
            outer: self.by_file.iter(),
            current_file: None,
            current: None,
        }
    }

    /// Replaces all entries belonging to one file.
    pub fn replace_file(
        &mut self,
        file_id: SourceFileId,
        entries: impl IntoIterator<Item = (TextRange, PositionRange)>,
    ) {
        self.remove_file(file_id);
        let entries = sort_position_entries(entries.into_iter().collect::<Vec<_>>());
        if entries.is_empty() {
            return;
        }
        self.len = self.len.saturating_add(entries.len());
        self.by_file.insert(file_id, entries);
    }

    /// Merges entries, replacing an existing value with the later value for a duplicate range.
    pub fn extend(
        &mut self,
        entries: impl IntoIterator<Item = ((SourceFileId, TextRange), PositionRange)>,
    ) {
        let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, PositionRange)>>::new();
        for ((file_id, range), position) in entries {
            grouped.entry(file_id).or_default().push((range, position));
        }
        self.merge_grouped(grouped);
    }

    /// Merges another map without flattening its already grouped vectors.
    pub fn merge(&mut self, other: Self) {
        self.merge_grouped(other.by_file);
    }

    fn merge_grouped(&mut self, grouped: BTreeMap<SourceFileId, Vec<(TextRange, PositionRange)>>) {
        for (file_id, additions) in grouped {
            let mut merged = self.by_file.remove(&file_id).unwrap_or_default();
            merged.extend(additions);
            let merged = sort_position_entries(merged);
            self.by_file.insert(file_id, merged);
        }
        self.recount_len();
    }

    /// Replaces the complete map from a deterministic iterator.
    pub fn from_entries(
        entries: impl IntoIterator<Item = ((SourceFileId, TextRange), PositionRange)>,
    ) -> Self {
        let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, PositionRange)>>::new();
        for ((file_id, range), position) in entries {
            grouped.entry(file_id).or_default().push((range, position));
        }
        Self::from_grouped(grouped)
    }

    /// Builds a map from already file-grouped entries without another intermediate grouping.
    pub(crate) fn from_grouped(
        grouped: BTreeMap<SourceFileId, Vec<(TextRange, PositionRange)>>,
    ) -> Self {
        let mut map = Self {
            by_file: BTreeMap::new(),
            len: 0,
        };
        for (file_id, entries) in grouped {
            let entries = sort_position_entries(entries);
            map.len = map.len.saturating_add(entries.len());
            if !entries.is_empty() {
                map.by_file.insert(file_id, entries);
            }
        }
        map
    }

    /// Removes all entries belonging to one file.
    pub fn remove_file(&mut self, file_id: SourceFileId) {
        if let Some(entries) = self.by_file.remove(&file_id) {
            self.len = self.len.saturating_sub(entries.len());
        }
    }

    /// Retains files satisfying `keep`.
    pub fn retain_files(&mut self, mut keep: impl FnMut(SourceFileId) -> bool) {
        let removed = self
            .by_file
            .keys()
            .copied()
            .filter(|file_id| !keep(*file_id))
            .collect::<Vec<_>>();
        for file_id in removed {
            self.remove_file(file_id);
        }
    }

    fn recount_len(&mut self) {
        self.len = self.by_file.values().map(Vec::len).sum();
    }
}

impl From<BTreeMap<(SourceFileId, TextRange), PositionRange>> for PositionMap {
    fn from(entries: BTreeMap<(SourceFileId, TextRange), PositionRange>) -> Self {
        Self::from_entries(entries)
    }
}

impl<'map> IntoIterator for &'map PositionMap {
    type Item = ((SourceFileId, TextRange), &'map PositionRange);
    type IntoIter = PositionMapIter<'map>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator returned by [`PositionMap::iter`].
pub struct PositionMapIter<'map> {
    outer: std::collections::btree_map::Iter<'map, SourceFileId, Vec<(TextRange, PositionRange)>>,
    current_file: Option<SourceFileId>,
    current: Option<std::slice::Iter<'map, (TextRange, PositionRange)>>,
}

impl<'map> Iterator for PositionMapIter<'map> {
    type Item = ((SourceFileId, TextRange), &'map PositionRange);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let (Some(file_id), Some(entries)) = (self.current_file, self.current.as_mut())
                && let Some((range, position)) = entries.next()
            {
                return Some(((file_id, *range), position));
            }
            let (file_id, entries) = self.outer.next()?;
            self.current_file = Some(*file_id);
            self.current = Some(entries.iter());
        }
    }
}

/// Compact, file-grouped localisation previews for cache-backed hover results.
///
/// A flat `BTreeMap<(SourceFileId, TextRange), LocalisationPreview>` stores one tree entry for
/// every localisation line. Grouping ranges by file keeps lookup deterministic while avoiding
/// the per-entry tree-node overhead for the hundreds of thousands of Vanilla previews.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalisationPreviewMap {
    by_file: BTreeMap<SourceFileId, Vec<(TextRange, LocalisationPreview)>>,
    len: usize,
}

impl LocalisationPreviewMap {
    /// Creates an empty preview map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of retained previews.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no previews are retained.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Looks up one file/range pair.
    #[must_use]
    pub fn get<K>(&self, key: K) -> Option<&LocalisationPreview>
    where
        K: std::borrow::Borrow<(SourceFileId, TextRange)>,
    {
        let key = key.borrow();
        let entries = self.by_file.get(&key.0)?;
        let index = entries
            .binary_search_by_key(&key.1, |(range, _)| *range)
            .ok()?;
        entries.get(index).map(|(_, preview)| preview)
    }

    /// Iterates in stable `(file_id, range)` order.
    pub fn iter(&self) -> LocalisationPreviewMapIter<'_> {
        LocalisationPreviewMapIter {
            outer: self.by_file.iter(),
            current_file: None,
            current: None,
        }
    }

    /// Replaces all previews belonging to one file.
    pub fn replace_file(
        &mut self,
        file_id: SourceFileId,
        entries: impl IntoIterator<Item = (TextRange, LocalisationPreview)>,
    ) {
        self.remove_file(file_id);
        let entries = sort_preview_entries(entries.into_iter().collect::<Vec<_>>());
        if entries.is_empty() {
            return;
        }
        self.len = self.len.saturating_add(entries.len());
        self.by_file.insert(file_id, entries);
    }

    /// Merges entries, replacing an existing value with the later value for a duplicate range.
    pub fn extend(
        &mut self,
        entries: impl IntoIterator<Item = ((SourceFileId, TextRange), LocalisationPreview)>,
    ) {
        let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, LocalisationPreview)>>::new();
        for ((file_id, range), preview) in entries {
            grouped.entry(file_id).or_default().push((range, preview));
        }
        self.merge_grouped(grouped);
    }

    /// Merges another map without flattening its already grouped vectors.
    pub fn merge(&mut self, other: Self) {
        self.merge_grouped(other.by_file);
    }

    fn merge_grouped(
        &mut self,
        grouped: BTreeMap<SourceFileId, Vec<(TextRange, LocalisationPreview)>>,
    ) {
        for (file_id, additions) in grouped {
            let mut merged = self.by_file.remove(&file_id).unwrap_or_default();
            merged.extend(additions);
            let merged = sort_preview_entries(merged);
            self.by_file.insert(file_id, merged);
        }
        self.recount_len();
    }

    /// Builds a map from a deterministic iterator.
    #[must_use]
    pub fn from_entries(
        entries: impl IntoIterator<Item = ((SourceFileId, TextRange), LocalisationPreview)>,
    ) -> Self {
        let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, LocalisationPreview)>>::new();
        for ((file_id, range), preview) in entries {
            grouped.entry(file_id).or_default().push((range, preview));
        }
        Self::from_grouped(grouped)
    }

    /// Builds a map from already file-grouped entries without another intermediate grouping.
    pub(crate) fn from_grouped(
        grouped: BTreeMap<SourceFileId, Vec<(TextRange, LocalisationPreview)>>,
    ) -> Self {
        let mut map = Self {
            by_file: BTreeMap::new(),
            len: 0,
        };
        for (file_id, entries) in grouped {
            let entries = sort_preview_entries(entries);
            map.len = map.len.saturating_add(entries.len());
            if !entries.is_empty() {
                map.by_file.insert(file_id, entries);
            }
        }
        map
    }

    /// Removes all previews belonging to one file.
    pub fn remove_file(&mut self, file_id: SourceFileId) {
        if let Some(entries) = self.by_file.remove(&file_id) {
            self.len = self.len.saturating_sub(entries.len());
        }
    }

    /// Retains files satisfying `keep`.
    pub fn retain_files(&mut self, mut keep: impl FnMut(SourceFileId) -> bool) {
        let removed = self
            .by_file
            .keys()
            .copied()
            .filter(|file_id| !keep(*file_id))
            .collect::<Vec<_>>();
        for file_id in removed {
            self.remove_file(file_id);
        }
    }

    fn recount_len(&mut self) {
        self.len = self.by_file.values().map(Vec::len).sum();
    }
}

impl<'map> IntoIterator for &'map LocalisationPreviewMap {
    type Item = ((SourceFileId, TextRange), &'map LocalisationPreview);
    type IntoIter = LocalisationPreviewMapIter<'map>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl From<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>> for LocalisationPreviewMap {
    fn from(entries: BTreeMap<(SourceFileId, TextRange), LocalisationPreview>) -> Self {
        Self::from_entries(entries)
    }
}

/// Iterator returned by [`LocalisationPreviewMap::iter`].
pub struct LocalisationPreviewMapIter<'map> {
    outer: std::collections::btree_map::Iter<
        'map,
        SourceFileId,
        Vec<(TextRange, LocalisationPreview)>,
    >,
    current_file: Option<SourceFileId>,
    current: Option<std::slice::Iter<'map, (TextRange, LocalisationPreview)>>,
}

impl<'map> Iterator for LocalisationPreviewMapIter<'map> {
    type Item = ((SourceFileId, TextRange), &'map LocalisationPreview);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let (Some(file_id), Some(entries)) = (self.current_file, self.current.as_mut())
                && let Some((range, preview)) = entries.next()
            {
                return Some(((file_id, *range), preview));
            }
            let (file_id, entries) = self.outer.next()?;
            self.current_file = Some(*file_id);
            self.current = Some(entries.iter());
        }
    }
}

fn sort_preview_entries(
    mut entries: Vec<(TextRange, LocalisationPreview)>,
) -> Vec<(TextRange, LocalisationPreview)> {
    entries.sort_by_key(|(range, _)| *range);
    let mut write = 0;
    for read in 0..entries.len() {
        if write > 0 && entries[write - 1].0 == entries[read].0 {
            entries[write - 1] = entries[read].clone();
        } else {
            entries.swap(write, read);
            write += 1;
        }
    }
    entries.truncate(write);
    entries
}

fn sort_position_entries(
    mut entries: Vec<(TextRange, PositionRange)>,
) -> Vec<(TextRange, PositionRange)> {
    entries.sort_by_key(|(range, _)| *range);
    let mut write = 0;
    for read in 0..entries.len() {
        if write > 0 && entries[write - 1].0 == entries[read].0 {
            entries[write - 1] = entries[read];
        } else {
            entries.swap(write, read);
            write += 1;
        }
    }
    entries.truncate(write);
    entries
}

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
    /// Semantic kind, for example event or localisation. Interned through the
    /// process-wide pool: kinds repeat for every definition in the workspace.
    pub kind: Arc<str>,
    /// Symbol name as written in source, interned through the process pool.
    pub name: Arc<str>,
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
    /// Reference category, interned through the process pool.
    pub kind: Arc<str>,
    /// Referenced name, interned through the process pool.
    pub name: Arc<str>,
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
    /// Priority-resolution result for this entry, tracked on the pointer so
    /// shards stay immutable and can be shared by `Arc` between the index and
    /// file states. Initialized from the shard definition's own flag.
    active: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceIndex {
    pub(crate) shards: BTreeMap<SourceFileId, Arc<FileIndexShard>>,
    /// Nested so lookups probe with borrowed strings: kind spellings as written, folded
    /// names inside. Nested BTree iteration preserves the previous `(kind, name)` order.
    definitions: BTreeMap<Box<str>, BTreeMap<Box<str>, Vec<DefinitionPointer>>>,
    case_sensitive_kinds: BTreeSet<String>,
    /// Cached UTF-16 positions for files whose source text is not retained, such as Vanilla.
    pub(crate) position_ranges: PositionMap,
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
    pub fn from_shards(shards: impl IntoIterator<Item: Into<Arc<FileIndexShard>>>) -> Self {
        match Self::from_shards_cancellable(shards, &WorkspaceScanToken::new()) {
            Ok(index) => index,
            Err(WorkspaceError::Cancelled) => {
                unreachable!("a fresh workspace scan token cannot be cancelled")
            }
            Err(_) => unreachable!("index construction has no other fallible operation"),
        }
    }

    /// Builds an index with the symbol case policy applied in the same single pass.
    #[must_use]
    pub fn from_shards_with_rules(
        shards: impl IntoIterator<Item: Into<Arc<FileIndexShard>>>,
        rules: &RuleSet,
    ) -> Self {
        match Self::from_shards_cancellable_with_rules(shards, rules, &WorkspaceScanToken::new()) {
            Ok(index) => index,
            Err(WorkspaceError::Cancelled) => {
                unreachable!("a fresh workspace scan token cannot be cancelled")
            }
            Err(_) => unreachable!("index construction has no other fallible operation"),
        }
    }

    fn from_shards_cancellable(
        shards: impl IntoIterator<Item: Into<Arc<FileIndexShard>>>,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, WorkspaceError> {
        let mut index = Self::empty();
        for shard in shards {
            cancellation.checkpoint()?;
            let shard = shard.into();
            index.shards.insert(shard.file_id, shard);
        }
        index.rebuild_maps_cancellable(cancellation)?;
        Ok(index)
    }

    pub(crate) fn from_shards_cancellable_with_rules(
        shards: impl IntoIterator<Item: Into<Arc<FileIndexShard>>>,
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
            let shard = shard.into();
            index.shards.insert(shard.file_id, shard);
        }
        index.rebuild_maps_cancellable(cancellation)?;
        Ok(index)
    }

    /// Returns retained definitions for a kind/name with their live
    /// priority-resolution state from the pointer buckets.
    #[must_use]
    pub fn definitions_with_state(&self, kind: &str, name: &str) -> Vec<(&Definition, bool)> {
        self.definition_bucket(kind, name)
            .iter()
            .filter_map(|pointer| {
                self.definition_at(*pointer)
                    .map(|definition| (definition, pointer.active))
            })
            .collect()
    }

    /// Returns the pointer bucket for one `(kind, name)` without allocating a key.
    fn definition_bucket(&self, kind: &str, name: &str) -> &[DefinitionPointer] {
        self.definitions
            .get(kind)
            .and_then(|by_name| by_name.get(self.lookup_name(kind, name).as_ref()))
            .map_or(&[][..], |bucket| bucket.as_slice())
    }

    /// Returns all retained definitions for a kind/name, including shadowed ones.
    #[must_use]
    pub fn definitions(&self, kind: &str, name: &str) -> Vec<&Definition> {
        self.definition_bucket(kind, name)
            .iter()
            .filter_map(|pointer| self.definition_at(*pointer))
            .collect()
    }

    /// Returns the active definition for a kind/name, if one exists.
    #[must_use]
    pub fn active_definition(&self, kind: &str, name: &str) -> Option<&Definition> {
        let definitions = self.definitions_with_state(kind, name);
        let mut active = definitions
            .into_iter()
            .filter(|(_definition, is_active)| *is_active)
            .map(|(definition, _)| definition);
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
            .flat_map(|by_name| by_name.values())
            .flatten()
            .filter_map(|pointer| self.definition_at(*pointer))
    }

    /// Iterates every retained definition pointer together with its resolution activity.
    ///
    /// Lets analysis layers build their own membership views (for example a per-revision
    /// `(kind, name)` set) in one pass instead of one bucket probe per query.
    #[must_use = "iterate the retained definition identities"]
    pub fn definition_identities(&self) -> impl Iterator<Item = (&Definition, bool)> {
        self.definitions
            .values()
            .flat_map(|by_name| by_name.values())
            .flatten()
            .filter_map(|pointer| self.definition_at(*pointer).map(|d| (d, d.active)))
    }

    /// Returns the name folding applied by definition lookups for one kind.
    ///
    /// Case-sensitive kinds keep their spelling; every other kind folds to lowercase. Public
    /// so higher layers can precompute membership keys exactly as [`Self::definitions`] does.
    #[must_use]
    pub fn definition_name_key<'name>(
        &self,
        kind: &str,
        name: &'name str,
    ) -> std::borrow::Cow<'name, str> {
        if self.is_case_sensitive(kind) {
            std::borrow::Cow::Borrowed(name)
        } else if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(name.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        }
    }

    /// Iterates over retained definitions of one exact kind without scanning unrelated symbols.
    #[must_use = "iterate the retained definitions"]
    pub fn definitions_for_kind<'index>(
        &'index self,
        kind: &str,
    ) -> impl Iterator<Item = &'index Definition> {
        let kind_bound: Box<str> = Box::from(kind);
        self.definitions
            .range(kind_bound..)
            .take_while(move |(candidate_kind, _)| candidate_kind.as_ref() == kind)
            .flat_map(|(_, by_name)| by_name.values())
            .flatten()
            .filter_map(|pointer| self.definition_at(*pointer))
    }

    /// Returns the shard for a file.
    #[must_use]
    pub fn shard(&self, file_id: SourceFileId) -> Option<&FileIndexShard> {
        self.shards.get(&file_id).map(Arc::as_ref)
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
        self.position_ranges.get((file_id, range)).copied()
    }

    /// Returns all cached editor positions retained by this index.
    #[must_use]
    pub fn position_ranges(&self) -> &PositionMap {
        &self.position_ranges
    }

    /// Replaces cached editor positions for one source file.
    pub fn replace_position_ranges(
        &mut self,
        file_id: SourceFileId,
        positions: impl IntoIterator<Item = (TextRange, PositionRange)>,
    ) {
        self.position_ranges.replace_file(file_id, positions);
    }

    /// Replaces all cached editor positions.
    pub fn replace_all_position_ranges(&mut self, positions: impl Into<PositionMap>) {
        self.position_ranges = positions.into();
    }

    /// Removes cached editor positions for one source file.
    pub fn remove_position_ranges(&mut self, file_id: SourceFileId) {
        self.position_ranges.remove_file(file_id);
    }

    fn definition_at(&self, pointer: DefinitionPointer) -> Option<&Definition> {
        self.shards
            .get(&pointer.file_id)?
            .definitions
            .get(pointer.ordinal)
    }

    /// Replaces one shard and updates only lookup buckets touched by that file.
    pub fn replace_shard(&mut self, shard: FileIndexShard) {
        self.replace_shard_entries(Arc::new(shard));
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
        let policies = symbol_policies(rules);
        self.resolve_definition_buckets(&affected, priorities, &policies);
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
        let keys = self
            .definitions
            .iter()
            .flat_map(|(kind, by_name)| {
                by_name.keys().map(move |name| (kind.clone(), name.clone()))
            })
            .collect::<Vec<_>>();
        let policies = symbol_policies(rules);
        for key in &keys {
            cancellation.checkpoint()?;
            self.resolve_definition_buckets(std::slice::from_ref(key), priorities, &policies);
        }
        Ok(())
    }

    pub(crate) fn replace_shard_resolved(
        &mut self,
        shard: Arc<FileIndexShard>,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        let affected = self.replace_shard_entries(shard);
        let policies = symbol_policies(rules);
        self.resolve_definition_buckets(&affected, priorities, &policies);
    }

    fn replace_shard_entries(&mut self, shard: Arc<FileIndexShard>) -> Vec<(Box<str>, Box<str>)> {
        let file_id = shard.file_id;
        let mut affected = self.remove_shard_entries(file_id);
        for (ordinal, definition) in shard.definitions.iter().enumerate() {
            let key = self.definition_key(definition);
            self.definitions
                .entry(key.0.clone())
                .or_default()
                .entry(key.1.clone())
                .or_default()
                .push(DefinitionPointer {
                    file_id,
                    ordinal,
                    active: definition.active,
                });
            affected.push(key);
        }
        self.shards.insert(file_id, shard);
        affected.sort();
        affected.dedup();
        self.sort_definition_buckets(&affected);
        affected
    }

    fn remove_shard_entries(&mut self, file_id: SourceFileId) -> Vec<(Box<str>, Box<str>)> {
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
            let remove_bucket = self
                .definitions
                .get_mut(key.0.as_ref())
                .and_then(|by_name| by_name.get_mut(key.1.as_ref()))
                .is_some_and(|definitions| {
                    definitions.retain(|definition| definition.file_id != file_id);
                    definitions.is_empty()
                });
            if remove_bucket && let Some(by_name) = self.definitions.get_mut(key.0.as_ref()) {
                by_name.remove(key.1.as_ref());
                if by_name.is_empty() {
                    self.definitions.remove(key.0.as_ref());
                }
            }
        }
        affected
    }

    fn resolve_definition_buckets(
        &mut self,
        keys: &[(Box<str>, Box<str>)],
        priorities: &BTreeMap<SourceFileId, u64>,
        policies: &BTreeMap<String, SymbolResolutionPolicy>,
    ) {
        for key in keys {
            let policy = policies
                .get(&key.0.to_ascii_lowercase())
                .copied()
                .unwrap_or(SymbolResolutionPolicy::ReplaceBySymbol);
            let Some(highest) = self
                .definitions
                .get(key.0.as_ref())
                .and_then(|by_name| by_name.get(key.1.as_ref()))
                .and_then(|values| {
                    values
                        .iter()
                        .map(|pointer| priorities.get(&pointer.file_id).copied().unwrap_or(0))
                        .max()
                })
            else {
                continue;
            };
            let updates = self
                .definitions
                .get(key.0.as_ref())
                .and_then(|by_name| by_name.get(key.1.as_ref()))
                .expect("checked above")
                .iter()
                .map(|pointer| {
                    let file_id = pointer.file_id;
                    self.shards
                        .get(&file_id)
                        .and_then(|shard| shard.definitions.get(pointer.ordinal))
                        .map(|_definition| match policy {
                            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
                            SymbolResolutionPolicy::ReplaceBySymbol => {
                                Some(priorities.get(&file_id).copied().unwrap_or(0))
                                    == Some(highest)
                            }
                        })
                })
                .collect::<Vec<_>>();
            if let Some(values) = self
                .definitions
                .get_mut(key.0.as_ref())
                .and_then(|by_name| by_name.get_mut(key.1.as_ref()))
            {
                for (pointer, update) in values.iter_mut().zip(updates) {
                    if let Some(active) = update {
                        pointer.active = active;
                    }
                }
            }
            self.sort_definition_buckets(std::slice::from_ref(key));
        }
    }

    fn sort_definition_buckets(&mut self, keys: &[(Box<str>, Box<str>)]) {
        for key in keys {
            if let Some(values) = self
                .definitions
                .get_mut(key.0.as_ref())
                .and_then(|by_name| by_name.get_mut(key.1.as_ref()))
            {
                values.sort_by_key(|pointer| (!pointer.active, pointer.file_id));
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
                let (kind_key, name_key) = self.definition_key(definition);
                self.definitions
                    .entry(kind_key)
                    .or_default()
                    .entry(name_key)
                    .or_default()
                    .push(DefinitionPointer {
                        file_id: *file_id,
                        ordinal,
                        active: definition.active,
                    });
            }
        }
        for by_name in self.definitions.values_mut() {
            for values in by_name.values_mut() {
                cancellation.checkpoint()?;
                values.sort_by_key(|pointer| (!pointer.active, pointer.file_id));
            }
        }
        Ok(())
    }
}

/// Builds the kind -> resolution policy lookup used while resolving definition priorities.
fn symbol_policies(rules: &RuleSet) -> BTreeMap<String, SymbolResolutionPolicy> {
    rules
        .model()
        .symbol_descriptors
        .iter()
        .map(|descriptor| {
            (
                descriptor.kind_id.to_ascii_lowercase(),
                descriptor.resolution,
            )
        })
        .collect()
}

impl WorkspaceIndex {
    fn is_case_sensitive(&self, kind: &str) -> bool {
        // Kinds are stored (and queried) lowercase in practice; borrowing avoids the
        // per-probe lowercase allocation this membership hot path used to pay.
        if kind.bytes().any(|byte| byte.is_ascii_uppercase()) {
            self.case_sensitive_kinds
                .contains(&kind.to_ascii_lowercase())
        } else {
            self.case_sensitive_kinds.contains(kind)
        }
    }

    fn lookup_name<'name>(&self, kind: &str, name: &'name str) -> std::borrow::Cow<'name, str> {
        if self.is_case_sensitive(kind) {
            std::borrow::Cow::Borrowed(name)
        } else if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(name.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(name)
        }
    }

    fn definition_key(&self, definition: &Definition) -> (Box<str>, Box<str>) {
        let folded = self.lookup_name(&definition.kind, &definition.name);
        (
            Box::from(definition.kind.as_ref()),
            Box::from(folded.as_ref()),
        )
    }
}
