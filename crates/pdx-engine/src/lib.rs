//! Semantic engine: HIR lowering, workspace state, and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

#[cfg(test)]
use std::cell::Cell;
pub mod hir;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::hir::{HirFile, lower_shared, lower_shared_with_profile};
use encoding_rs::WINDOWS_1252;
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile, parse};
use pdx_rules::{
    FileResolutionPolicy, GameProfile, ParserKind, RuleSet, SourceEncoding, SymbolResolutionPolicy,
};
use pdx_text::{LineIndex, LogicalPath, PositionRange, TextRange};

mod vanilla_cache;

pub use vanilla_cache::{
    CURRENT_VANILLA_CACHE_SCHEMA_VERSION, VanillaCacheError, VanillaIndexCache,
    VanillaIndexCacheMetadata,
};

#[cfg(test)]
thread_local! {
    #[allow(clippy::missing_const_for_thread_local)]
    static PIPELINE_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

#[cfg(test)]
fn record_pipeline_parse() {
    PIPELINE_COUNTS.with(|counts| {
        let (parses, lowers) = counts.get();
        counts.set((parses.saturating_add(1), lowers));
    });
}

#[cfg(test)]
fn record_pipeline_lower() {
    PIPELINE_COUNTS.with(|counts| {
        let (parses, lowers) = counts.get();
        counts.set((parses, lowers.saturating_add(1)));
    });
}

#[cfg(test)]
fn reset_pipeline_counts() {
    PIPELINE_COUNTS.set((0, 0));
}

#[cfg(test)]
fn pipeline_counts() -> (usize, usize) {
    PIPELINE_COUNTS.get()
}

/// Stable identity for a source root during one host lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceRootId(u32);

impl SourceRootId {
    /// Creates an ID. Callers that allocate IDs should keep them stable.
    #[must_use = "iterate the retained definitions"]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A source root ordered by the future overlay resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRoot {
    /// Stable identity.
    pub id: SourceRootId,
    /// Root kind; resolution order is implemented in a later phase.
    pub kind: SourceRootKind,
    /// Explicit filesystem path.
    pub path: PathBuf,
    /// Explicit low-to-high order among roots of the same kind.
    pub order: u32,
    /// Whether this root is allowed to own generated or edited files.
    pub writable: bool,
}

impl SourceRoot {
    /// Creates a root with an order derived from its stable ID.
    #[must_use]
    pub fn new(id: SourceRootId, kind: SourceRootKind, path: PathBuf) -> Self {
        let writable = matches!(kind, SourceRootKind::CurrentMod);
        Self {
            id,
            kind,
            path,
            order: id.get(),
            writable,
        }
    }
}

/// Source-root category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceRootKind {
    /// The local Vanilla installation/cache.
    Vanilla,
    /// An explicitly ordered dependency Mod.
    Dependency,
    /// The current Mod being edited.
    CurrentMod,
}

/// Stable identity for a discovered source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceFileId(u64);

impl SourceFileId {
    /// Creates an ID from a stable root/path hash.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A discovered file candidate in one physical source root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    /// Stable file identity.
    pub id: SourceFileId,
    /// Owning source root.
    pub root_id: SourceRootId,
    /// Physical disk path.
    pub physical_path: PathBuf,
    /// PDX logical path relative to the root.
    pub logical_path: LogicalPath,
    /// Rules catalog category, when one matched.
    pub category_id: Option<String>,
    /// File resolution policy selected by the rules catalog.
    pub resolution: FileResolutionPolicy,
}

/// Resource boundaries applied while discovering source files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceScanLimits {
    /// Maximum number of nested directories below a configured source root.
    pub max_depth: usize,
    /// Maximum number of regular files examined across all source roots.
    pub max_files: usize,
    /// Maximum size of one source file in bytes.
    pub max_file_size: u64,
    /// Maximum number of detailed issues retained in a scan report.
    pub max_reported_issues: usize,
}

/// Shared cooperative-cancellation state for source-root discovery and indexing.
#[derive(Clone, Debug)]
pub struct WorkspaceScanToken {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    remaining_checkpoints: Arc<AtomicUsize>,
}

impl Default for WorkspaceScanToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            remaining_checkpoints: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }
}

impl WorkspaceScanToken {
    /// Creates an uncancelled scan token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels discovery, file materialization, and index work using this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn checkpoint(&self) -> Result<(), WorkspaceError> {
        #[cfg(test)]
        if self
            .remaining_checkpoints
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                if remaining != usize::MAX && remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_err_and(|remaining| remaining == 0)
        {
            self.cancel();
        }
        if self.is_cancelled() {
            Err(WorkspaceError::Cancelled)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn cancel_after(checkpoints: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            remaining_checkpoints: Arc::new(AtomicUsize::new(checkpoints)),
        }
    }
}

impl Default for WorkspaceScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_files: 100_000,
            max_file_size: 16 * 1024 * 1024,
            max_reported_issues: 256,
        }
    }
}

/// Classification of a recoverable source-root scan problem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceScanIssueKind {
    /// A directory entry was a symbolic link and was not followed.
    SymlinkSkipped,
    /// A subtree exceeded the configured nesting limit.
    DepthLimitExceeded,
    /// A nested directory could not be read.
    DirectoryUnreadable,
    /// One directory entry could not be inspected.
    DirectoryEntryUnreadable,
    /// Metadata for a regular file could not be read.
    MetadataUnreadable,
    /// A file exceeded the configured per-file size limit.
    FileTooLarge,
    /// A file disappeared or otherwise could not be read after discovery.
    FileUnreadable,
    /// Source bytes were not valid UTF-8 or the selected profile encoding.
    InvalidUtf8,
    /// Decoded text is not human-readable source, likely game-only encoded.
    NonTextContent,
}

/// One recoverable problem encountered during source-root discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceScanIssue {
    /// Stable machine-readable issue category.
    pub kind: WorkspaceScanIssueKind,
    /// Best available physical path for the affected entry.
    pub path: PathBuf,
    /// Human-readable detail suitable for logs or future diagnostics.
    pub detail: String,
}

/// Summary of the most recent successful bounded workspace scan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceScanReport {
    /// Regular files examined before rules classification.
    pub discovered_files: usize,
    /// Classified, readable files added to the workspace after decoding.
    pub indexed_files: usize,
    /// Source files decoded from the selected profile's legacy encoding.
    pub legacy_encoded_files: usize,
    /// Entries or subtrees skipped for a recoverable reason.
    pub skipped_entries: usize,
    /// Retained issue details, bounded by [`WorkspaceScanLimits::max_reported_issues`].
    pub issues: Vec<WorkspaceScanIssue>,
    /// Additional issues omitted after the report detail limit was reached.
    pub omitted_issues: usize,
}

/// A candidate retained by overlay resolution, including shadowed definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCandidate {
    /// Logical path.
    pub logical_path: LogicalPath,
    /// Disk file identity, if this is a disk candidate.
    pub file_id: Option<SourceFileId>,
    /// Overlay document identity, if this is an in-memory candidate.
    pub document_id: Option<DocumentId>,
    /// Candidate priority; larger values win.
    pub priority: u64,
    /// File policy used to determine whether lower candidates remain active.
    pub resolution: Option<FileResolutionPolicy>,
    /// Whether this candidate is active for its logical path.
    pub active: bool,
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
    /// Syntax error count retained as a cheap health signal.
    pub syntax_error_count: usize,
}

/// Bounded, derived text retained for a localisation Hover result.
///
/// This is intentionally not source text: Vanilla caches persist only this small preview so
/// normal startup can answer Hover without reopening the Vanilla installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalisationPreview {
    /// The most recent language header preceding the entry, when one was present.
    pub language: Option<String>,
    /// The bounded display value.
    pub value: String,
}

const MAX_LOCALISATION_PREVIEW_CHARS: usize = 240;

fn localisation_previews_from_parsed(parsed: &ParsedFile) -> Vec<(TextRange, LocalisationPreview)> {
    if parsed.format() != FileFormat::Localisation {
        return Vec::new();
    }
    let mut previews = Vec::new();
    let mut language = None;
    for node in parsed.root().children() {
        match node.kind() {
            CstKind::LanguageHeader => {
                language = node
                    .children()
                    .iter()
                    .find(|child| child.kind() == CstKind::LocalisationKey)
                    .and_then(|child| parsed.text(child.range()))
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty());
            }
            CstKind::LocalisationEntry => {
                let Some(value_node) = node.children().iter().find(|child| {
                    matches!(
                        child.kind(),
                        CstKind::LocalisationString | CstKind::UnquotedValue
                    )
                }) else {
                    continue;
                };
                let Some(raw) = parsed.text(value_node.range()).map(str::trim) else {
                    continue;
                };
                let value = raw
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(raw);
                let truncated = value.chars().count() > MAX_LOCALISATION_PREVIEW_CHARS;
                let mut value = value
                    .chars()
                    .take(MAX_LOCALISATION_PREVIEW_CHARS)
                    .collect::<String>();
                if truncated {
                    value.push('…');
                }
                if !value.is_empty() {
                    previews.push((
                        node.range(),
                        LocalisationPreview {
                            language: language.clone(),
                            value,
                        },
                    ));
                }
            }
            _ => {}
        }
    }
    previews
}

/// Parsed frontend retained by one immutable file state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedSource {
    /// Paradox script or localisation CST.
    Text(Arc<ParsedFile>),
}

impl ParsedSource {
    /// Returns the common frontend format.
    #[must_use]
    pub fn format(&self) -> FileFormat {
        match self {
            Self::Text(parsed) => parsed.format(),
        }
    }
}

/// Immutable parse/lower/index result for one disk file revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileState {
    revision: u64,
    source: Arc<str>,
    parsed: Option<ParsedSource>,
    hir: Option<Arc<HirFile>>,
    shard: Arc<FileIndexShard>,
    cached_positions: Option<Arc<Vec<(TextRange, PositionRange)>>>,
    cached_localisation_previews: Option<Arc<Vec<(TextRange, LocalisationPreview)>>>,
}

impl FileState {
    /// Returns the per-file revision. It changes only when this file state is rebuilt.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the source text retained by this state.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Clones the shared source handle without copying its text.
    #[must_use]
    pub fn source_handle(&self) -> Arc<str> {
        Arc::clone(&self.source)
    }

    /// Returns the cached parsed frontend, when this category has one.
    #[must_use]
    pub fn parsed(&self) -> Option<&ParsedSource> {
        self.parsed.as_ref()
    }

    /// Returns the cached HIR, when this frontend supports lowering.
    #[must_use]
    pub fn hir(&self) -> Option<&HirFile> {
        self.hir.as_deref()
    }

    /// Clones the shared HIR handle without rebuilding or copying it.
    #[must_use]
    pub fn hir_handle(&self) -> Option<Arc<HirFile>> {
        self.hir.as_ref().map(Arc::clone)
    }

    /// Returns the index shard produced atomically with this parse/HIR state.
    #[must_use]
    pub fn shard(&self) -> &FileIndexShard {
        &self.shard
    }

    fn cache_only(mut self, positions: Vec<(TextRange, PositionRange)>) -> Self {
        let cached_localisation_previews =
            self.cached_localisation_previews
                .take()
                .or_else(|| match self.parsed.as_ref() {
                    Some(ParsedSource::Text(parsed)) => {
                        let previews = localisation_previews_from_parsed(parsed);
                        (!previews.is_empty()).then(|| Arc::new(previews))
                    }
                    None => None,
                });
        Self {
            revision: self.revision,
            source: self.source,
            parsed: None,
            hir: None,
            shard: self.shard,
            cached_positions: Some(Arc::new(positions)),
            cached_localisation_previews,
        }
    }

    fn cache_only_from_existing(&self, positions: Vec<(TextRange, PositionRange)>) -> Self {
        let cached_localisation_previews = self
            .cached_localisation_previews
            .as_ref()
            .map(Arc::clone)
            .or_else(|| match self.parsed.as_ref() {
                Some(ParsedSource::Text(parsed)) => {
                    let previews = localisation_previews_from_parsed(parsed);
                    (!previews.is_empty()).then(|| Arc::new(previews))
                }
                None => None,
            });
        Self {
            revision: self.revision,
            source: Arc::clone(&self.source),
            parsed: None,
            hir: None,
            shard: Arc::clone(&self.shard),
            cached_positions: Some(Arc::new(positions)),
            cached_localisation_previews,
        }
    }

    pub(crate) fn cached_localisation_previews(
        &self,
    ) -> Option<&[(TextRange, LocalisationPreview)]> {
        self.cached_localisation_previews
            .as_deref()
            .map(Vec::as_slice)
    }
}

/// Workspace-wide symbol index made from immutable file shards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DefinitionPointer {
    file_id: SourceFileId,
    ordinal: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceIndex {
    shards: BTreeMap<SourceFileId, FileIndexShard>,
    definitions: BTreeMap<(String, String), Vec<DefinitionPointer>>,
    case_sensitive_kinds: BTreeSet<String>,
    /// Cached UTF-16 positions for files whose source text is not retained, such as Vanilla.
    position_ranges: BTreeMap<(SourceFileId, TextRange), PositionRange>,
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

    fn from_shards_cancellable_with_rules(
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

    fn remove_shard_resolved(
        &mut self,
        file_id: SourceFileId,
        priorities: &BTreeMap<SourceFileId, u64>,
        rules: &RuleSet,
    ) {
        let affected = self.remove_shard_entries(file_id);
        self.resolve_definition_buckets(&affected, priorities, rules);
    }

    fn resolve_priorities(&mut self, priorities: &BTreeMap<SourceFileId, u64>, rules: &RuleSet) {
        match self.resolve_priorities_cancellable(priorities, rules, &WorkspaceScanToken::new()) {
            Ok(()) => {}
            Err(WorkspaceError::Cancelled) => {
                unreachable!("a fresh workspace scan token cannot be cancelled")
            }
            Err(_) => unreachable!("priority resolution has no other fallible operation"),
        }
    }

    fn resolve_priorities_cancellable(
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

    fn replace_shard_resolved(
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

/// Stable identity for an editor document during one server lifetime.
///
/// The value is the client URI rather than a filesystem path. This keeps the identity stable for
/// unsaved and non-file documents while leaving URI/path conversion to `pdx-lsp`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentId(String);

impl DocumentId {
    /// Creates a document identity from its client URI.
    #[must_use]
    pub fn new(uri: impl Into<String>) -> Self {
        Self(uri.into())
    }

    /// Returns the client URI.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The current candidate for a document.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocumentSource {
    /// Unsaved editor text is currently overriding the backing candidate.
    Overlay,
    /// Text was recovered from the backing filesystem candidate after close.
    Disk,
}

/// A document candidate exposed by an immutable workspace snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSnapshot {
    id: DocumentId,
    version: Option<i64>,
    text: Arc<str>,
    line_index: LineIndex,
    source: DocumentSource,
    path: Option<PathBuf>,
    parsed: Option<ParsedSource>,
    hir: Option<Arc<HirFile>>,
}

/// A fully parsed overlay candidate prepared outside the mutable host.
#[derive(Clone, Debug)]
pub struct PreparedDocument {
    document: DocumentSnapshot,
}

impl PreparedDocument {
    /// Returns the document identity carried by this candidate.
    #[must_use]
    pub const fn id(&self) -> &DocumentId {
        &self.document.id
    }

    /// Returns the overlay version carried by this candidate.
    #[must_use]
    pub const fn version(&self) -> Option<i64> {
        self.document.version
    }
}

impl DocumentSnapshot {
    /// Returns the document identity.
    #[must_use]
    pub fn id(&self) -> &DocumentId {
        &self.id
    }

    /// Returns the editor version, or `None` for a disk candidate.
    #[must_use]
    pub const fn version(&self) -> Option<i64> {
        self.version
    }

    /// Returns the lossless document text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Clones the shared text handle without copying its contents.
    #[must_use]
    pub fn text_handle(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Returns the UTF-8/UTF-16 line index for this text.
    #[must_use]
    pub const fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// Returns whether this candidate is an overlay or disk text.
    #[must_use]
    pub const fn source(&self) -> DocumentSource {
        self.source
    }

    /// Returns the backing filesystem path, when this URI is a file URI.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Returns the parsed frontend built for this exact document version.
    #[must_use]
    pub fn parsed(&self) -> Option<&ParsedSource> {
        self.parsed.as_ref()
    }

    /// Returns the HIR built for this exact document version.
    #[must_use]
    pub fn hir(&self) -> Option<&HirFile> {
        self.hir.as_deref()
    }

    /// Clones the HIR handle for this exact document version.
    #[must_use]
    pub fn hir_handle(&self) -> Option<Arc<HirFile>> {
        self.hir.as_ref().map(Arc::clone)
    }
}

/// One editor-neutral document change. Ranges use UTF-8 byte offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextChange {
    /// A byte range to replace, or `None` for a full-document replacement.
    pub range: Option<TextRange>,
    /// Replacement text.
    pub text: String,
}

impl TextChange {
    /// Creates a full-document replacement.
    #[must_use]
    pub fn full(text: impl Into<String>) -> Self {
        Self {
            range: None,
            text: text.into(),
        }
    }

    /// Creates a ranged replacement.
    #[must_use]
    pub fn ranged(range: TextRange, text: impl Into<String>) -> Self {
        Self {
            range: Some(range),
            text: text.into(),
        }
    }
}

/// A change applied by the event loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceChange {
    /// Replace the configured source roots.
    SetSourceRoots(Vec<SourceRoot>),
    /// Replace the explicit workspace root.
    SetWorkspaceRoot(Option<PathBuf>),
}

/// Filesystem event kind supplied by an editor watcher or save fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskFileChangeKind {
    /// A regular file appeared below a live source root.
    Created,
    /// An existing regular file may have changed contents.
    Changed,
    /// A path disappeared. A racing missing changed file is treated the same way.
    Deleted,
}

/// One editor-neutral disk candidate event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskFileChange {
    /// Absolute path reported by the client.
    pub path: PathBuf,
    /// Reported change kind.
    pub kind: DiskFileChangeKind,
}

impl DiskFileChange {
    /// Creates one disk event.
    #[must_use]
    pub fn new(path: PathBuf, kind: DiskFileChangeKind) -> Self {
        Self { path, kind }
    }
}

/// Errors raised while applying an editor document event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    /// An open notification was received for a document that is already open.
    AlreadyOpen(DocumentId),
    /// A change or close notification targeted no open overlay.
    NotOpen(DocumentId),
    /// A change version was not newer than the current version.
    StaleVersion {
        document: DocumentId,
        current: i64,
        received: i64,
    },
    /// A change range was not on UTF-8 boundaries or exceeded the current text.
    InvalidRange {
        document: DocumentId,
        range: TextRange,
    },
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOpen(document) => {
                write!(formatter, "document is already open: {}", document.as_str())
            }
            Self::NotOpen(document) => {
                write!(formatter, "document is not open: {}", document.as_str())
            }
            Self::StaleVersion {
                document,
                current,
                received,
            } => write!(
                formatter,
                "stale document version for {}: current {}, received {}",
                document.as_str(),
                current,
                received
            ),
            Self::InvalidRange { document, range } => write!(
                formatter,
                "invalid UTF-8 document range {}..{} for {}",
                range.start(),
                range.end(),
                document.as_str()
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Errors raised while materializing source roots and index shards.
#[derive(Debug)]
pub enum WorkspaceError {
    /// The caller cancelled source discovery or index materialization.
    Cancelled,
    /// Filesystem discovery or read failure.
    Io(std::io::Error),
    /// A root-relative path escaped its logical root.
    InvalidLogicalPath(PathBuf),
    /// Two distinct physical files produced the same stable source identity.
    FileIdCollision { first: PathBuf, second: PathBuf },
    /// Source discovery exceeded its total regular-file budget.
    FileLimitExceeded { limit: usize },
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("workspace scan was cancelled"),
            Self::Io(error) => write!(formatter, "workspace I/O error: {error}"),
            Self::InvalidLogicalPath(path) => {
                write!(
                    formatter,
                    "invalid workspace logical path: {}",
                    path.display()
                )
            }
            Self::FileIdCollision { first, second } => write!(
                formatter,
                "source file identity collision between {} and {}",
                first.display(),
                second.display()
            ),
            Self::FileLimitExceeded { limit } => {
                write!(
                    formatter,
                    "workspace contains more than the allowed {limit} files"
                )
            }
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn record_scan_issue(
    report: &mut WorkspaceScanReport,
    limits: WorkspaceScanLimits,
    kind: WorkspaceScanIssueKind,
    path: PathBuf,
    detail: String,
) {
    report.skipped_entries = report.skipped_entries.saturating_add(1);
    if report.issues.len() < limits.max_reported_issues {
        report
            .issues
            .push(WorkspaceScanIssue { kind, path, detail });
    } else {
        report.omitted_issues = report.omitted_issues.saturating_add(1);
    }
}

fn collect_whitelisted_files(
    root: &std::path::Path,
    profile: &GameProfile,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, PathBuf)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    let root_metadata = fs::metadata(root).map_err(WorkspaceError::Io)?;
    if !root_metadata.is_dir() {
        return Err(WorkspaceError::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!(
                "workspace source root is not a directory: {}",
                root.display()
            ),
        )));
    }

    let mut roots = profile
        .scan_roots()
        .iter()
        .map(|scan_root| {
            LogicalPath::parse(scan_root)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(PathBuf::from(scan_root)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    roots.sort();
    roots.dedup();
    let mut collapsed_roots = Vec::with_capacity(roots.len());
    for scan_root in roots {
        if collapsed_roots.iter().any(|parent: &LogicalPath| {
            parent.as_str() == scan_root.as_str()
                || scan_root
                    .as_str()
                    .strip_prefix(parent.as_str())
                    .is_some_and(|remainder| remainder.starts_with('/'))
        }) {
            continue;
        }
        collapsed_roots.push(scan_root);
    }

    let mut seen = BTreeSet::new();
    let mut scan = DiskScanContext {
        limits,
        profile,
        report,
        output,
        seen: &mut seen,
        cancellation,
    };
    for scan_root in collapsed_roots {
        scan.cancellation.checkpoint()?;
        let depth = scan_root
            .as_str()
            .split('/')
            .filter(|component| !component.is_empty())
            .count();
        let current = if scan_root.as_str().is_empty() {
            root.to_owned()
        } else {
            root.join(scan_root.as_str())
        };
        if depth > limits.max_depth {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::DepthLimitExceeded,
                current,
                format!(
                    "whitelisted directory depth exceeds the configured limit of {}",
                    limits.max_depth
                ),
            );
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryUnreadable,
                    current,
                    error.to_string(),
                );
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::SymlinkSkipped,
                current,
                "symbolic links are not followed during workspace discovery".to_owned(),
            );
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        collect_disk_files(root, &current, depth, &mut scan)?;
    }
    Ok(())
}

struct DiskScanContext<'a> {
    limits: WorkspaceScanLimits,
    profile: &'a GameProfile,
    report: &'a mut WorkspaceScanReport,
    output: &'a mut Vec<(LogicalPath, PathBuf)>,
    seen: &'a mut BTreeSet<LogicalPath>,
    cancellation: &'a WorkspaceScanToken,
}

fn collect_disk_files(
    root: &std::path::Path,
    current: &std::path::Path,
    depth: usize,
    scan: &mut DiskScanContext<'_>,
) -> Result<(), WorkspaceError> {
    scan.cancellation.checkpoint()?;
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if depth == 0 => return Err(WorkspaceError::Io(error)),
        Err(error) => {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::DirectoryUnreadable,
                current.to_owned(),
                error.to_string(),
            );
            return Ok(());
        }
    };
    let mut entries = entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    current.to_owned(),
                    error.to_string(),
                );
                None
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        scan.cancellation.checkpoint()?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    path,
                    error.to_string(),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::SymlinkSkipped,
                path,
                "symbolic links are not followed during workspace discovery".to_owned(),
            );
            continue;
        }
        if file_type.is_dir() {
            if ignored_workspace_directory(&entry.file_name()) {
                continue;
            }
            if depth >= scan.limits.max_depth {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DepthLimitExceeded,
                    path,
                    format!(
                        "directory nesting exceeds the configured limit of {}",
                        scan.limits.max_depth
                    ),
                );
                continue;
            }
            collect_disk_files(root, &path, depth + 1, scan)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if scan.report.discovered_files >= scan.limits.max_files {
            return Err(WorkspaceError::FileLimitExceeded {
                limit: scan.limits.max_files,
            });
        }
        scan.report.discovered_files = scan.report.discovered_files.saturating_add(1);
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !scan.profile.allows_scan_file(&relative) {
            continue;
        }
        let logical = LogicalPath::parse(&relative)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?;
        if !scan.seen.insert(logical.clone()) {
            continue;
        }
        scan.output.push((logical, path));
    }
    Ok(())
}

fn ignored_workspace_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".hg" | ".svn" | "node_modules" | "target")
    )
}

fn read_source_file(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    source_encoding: SourceEncoding,
) -> Option<String> {
    read_source_file_cancellable(
        path,
        limits,
        report,
        &WorkspaceScanToken::new(),
        source_encoding,
    )
    .ok()
    .flatten()
}

fn read_source_file_cancellable(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    cancellation: &WorkspaceScanToken,
    source_encoding: SourceEncoding,
) -> Result<Option<String>, WorkspaceError> {
    cancellation.checkpoint()?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                path.to_owned(),
                error.to_string(),
            );
            return Ok(None);
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::MetadataUnreadable,
                path.to_owned(),
                error.to_string(),
            );
            return Ok(None);
        }
    };
    if !metadata.is_file() {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileUnreadable,
            path.to_owned(),
            "source path is not a regular file".to_owned(),
        );
        return Ok(None);
    }
    if metadata.len() > limits.max_file_size {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileTooLarge,
            path.to_owned(),
            format!(
                "file size {} exceeds the configured limit of {} bytes",
                metadata.len(),
                limits.max_file_size
            ),
        );
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    if let Err(error) = file
        .take(limits.max_file_size.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileUnreadable,
            path.to_owned(),
            error.to_string(),
        );
        return Ok(None);
    }
    cancellation.checkpoint()?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > limits.max_file_size) {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileTooLarge,
            path.to_owned(),
            format!(
                "file grew beyond the configured limit of {} bytes",
                limits.max_file_size
            ),
        );
        return Ok(None);
    }
    let mut legacy = false;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let detail = error.to_string();
            let bytes = error.into_bytes();
            if source_encoding == SourceEncoding::Windows1252
                && looks_like_legacy_text(&bytes)
                && windows1252_has_no_undefined_bytes(&bytes)
            {
                let (text, had_errors) = WINDOWS_1252.decode_without_bom_handling(&bytes);
                if !had_errors {
                    legacy = true;
                    text.into_owned()
                } else {
                    record_scan_issue(
                        report,
                        limits,
                        WorkspaceScanIssueKind::InvalidUtf8,
                        path.to_owned(),
                        detail,
                    );
                    return Ok(None);
                }
            } else {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::InvalidUtf8,
                    path.to_owned(),
                    detail,
                );
                return Ok(None);
            }
        }
    };
    if contains_control_characters(&text) {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::NonTextContent,
            path.to_owned(),
            "decoded text contains control characters and is not human-readable source (likely game-only encoded)"
                .to_owned(),
        );
        return Ok(None);
    }
    if legacy {
        report.legacy_encoded_files = report.legacy_encoded_files.saturating_add(1);
    }
    Ok(Some(text))
}

fn contains_control_characters(text: &str) -> bool {
    text.chars()
        .any(|character| character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
}

fn looks_like_legacy_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0)
        && bytes
            .iter()
            .any(|byte| matches!(*byte, b'=' | b'{' | b'}' | b'#' | b'\n' | b':'))
}

fn windows1252_has_no_undefined_bytes(bytes: &[u8]) -> bool {
    !bytes
        .iter()
        .any(|byte| matches!(*byte, 0x81 | 0x8d | 0x8f | 0x90 | 0x9d))
}

fn stable_file_id(root: SourceRootId, logical: &LogicalPath) -> u64 {
    let mut value = 0xcbf29ce484222325_u64 ^ u64::from(root.get());
    for byte in logical.as_str().bytes() {
        value = (value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    value
}

fn root_priority(root: &SourceRoot) -> u64 {
    match root.kind {
        SourceRootKind::Vanilla => 0,
        SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}

fn source_priorities(
    roots: &[SourceRoot],
    files: &BTreeMap<SourceFileId, SourceFile>,
) -> BTreeMap<SourceFileId, u64> {
    files
        .values()
        .filter_map(|file| {
            roots
                .iter()
                .find(|root| root.id == file.root_id)
                .map(|root| (file.id, root_priority(root)))
        })
        .collect()
}

fn parse_source(
    parser: &ParserKind,
    source: &str,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: &GameProfile,
) -> (Option<ParsedSource>, Option<Arc<HirFile>>) {
    match parser {
        ParserKind::Script => {
            #[cfg(test)]
            record_pipeline_parse();
            let parsed = Arc::new(parse(FileFormat::Script, source));
            #[cfg(test)]
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Localisation => {
            #[cfg(test)]
            record_pipeline_parse();
            let parsed = Arc::new(parse(FileFormat::Localisation, source));
            #[cfg(test)]
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Asset | ParserKind::SyntaxOnly => (None, None),
    }
}

fn parser_for_document(
    rules: &RuleSet,
    roots: &[SourceRoot],
    id: &DocumentId,
    path: Option<&Path>,
) -> Option<(ParserKind, Option<LogicalPath>)> {
    let logical = path
        .and_then(|path| {
            roots
                .iter()
                .filter_map(|root| path.strip_prefix(&root.path).ok())
                .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
                .min_by_key(|path| path.as_str().len())
        })
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    if let Some(category) = logical.as_ref().and_then(|path| rules.classify(path)) {
        return Some((category.parser.clone(), logical));
    }
    let extension = path
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            logical.as_ref().and_then(|path| {
                path.as_str()
                    .rsplit_once('.')
                    .map(|(_, ext)| ext.to_ascii_lowercase())
            })
        })?;
    let parser = match extension.as_str() {
        "yml" | "yaml" => ParserKind::Localisation,
        "txt" | "gui" | "gfx" | "asset" | "sfx" => ParserKind::Script,
        _ => return None,
    };
    Some((parser, logical))
}

fn prepare_document_snapshot(
    rules: &RuleSet,
    profile: &GameProfile,
    roots: &[SourceRoot],
    mut document: DocumentSnapshot,
) -> DocumentSnapshot {
    let (parsed, hir) = parser_for_document(rules, roots, &document.id, document.path.as_deref())
        .map_or((None, None), |(parser, logical_path)| {
            parse_source(
                &parser,
                &document.text,
                logical_path.as_ref(),
                rules,
                profile,
            )
        });
    document.parsed = parsed;
    document.hir = hir;
    document
}

fn unparsed_document(
    id: DocumentId,
    version: Option<i64>,
    text: String,
    source: DocumentSource,
    path: Option<PathBuf>,
) -> DocumentSnapshot {
    let line_index = LineIndex::new(&text);
    DocumentSnapshot {
        id,
        version,
        text: Arc::from(text),
        line_index,
        source,
        path,
        parsed: None,
        hir: None,
    }
}

fn staged_overlay_document(
    id: DocumentId,
    version: i64,
    text: String,
    path: Option<PathBuf>,
) -> DocumentSnapshot {
    unparsed_document(id, Some(version), text, DocumentSource::Overlay, path)
}

const MAX_SOURCE_WORKERS: usize = 12;
const PARALLEL_SOURCE_THRESHOLD: usize = 32;

struct SourceReadJob {
    file: SourceFile,
    physical_path: PathBuf,
    retain_frontend: bool,
}

struct SourceReadResult {
    file: SourceFile,
    state: Option<Arc<FileState>>,
    report: WorkspaceScanReport,
}

struct SourceLoadContext<'a> {
    limits: WorkspaceScanLimits,
    previous_files: &'a BTreeMap<SourceFileId, SourceFile>,
    previous_states: &'a BTreeMap<SourceFileId, Arc<FileState>>,
    rules: &'a RuleSet,
    profile: &'a GameProfile,
    cancellation: &'a WorkspaceScanToken,
    progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

fn load_source_files(
    jobs: Vec<SourceReadJob>,
    files: &mut BTreeMap<SourceFileId, SourceFile>,
    file_states: &mut BTreeMap<SourceFileId, Arc<FileState>>,
    report: &mut WorkspaceScanReport,
    context: &SourceLoadContext<'_>,
) -> Result<(), WorkspaceError> {
    let total = jobs.len();
    let worker_count = thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .min(MAX_SOURCE_WORKERS)
        .min(jobs.len());
    let results = if jobs.len() < PARALLEL_SOURCE_THRESHOLD || worker_count < 2 {
        let mut results = Vec::with_capacity(jobs.len());
        let mut done = 0usize;
        for job in jobs {
            context.cancellation.checkpoint()?;
            results.push(load_source_file_job(job, context)?);
            done += 1;
            if let Some(progress) = context.progress {
                progress(done, total);
            }
        }
        results
    } else {
        let queue = Arc::new(Mutex::new(
            jobs.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut results = BTreeMap::new();
        thread::scope(|scope| -> Result<(), WorkspaceError> {
            let mut workers = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let queue = Arc::clone(&queue);
                let completed = Arc::clone(&completed);
                workers.push(scope.spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let job = match queue.lock() {
                            Ok(mut queue) => queue.pop_front(),
                            Err(_) => {
                                return Err(WorkspaceError::Io(std::io::Error::other(
                                    "workspace source worker queue was poisoned",
                                )));
                            }
                        };
                        let Some((index, job)) = job else {
                            break;
                        };
                        let result = load_source_file_job(job, context)?;
                        let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if let Some(progress) = context.progress {
                            progress(done, total);
                        }
                        results.push((index, result));
                    }
                    Ok(results)
                }));
            }
            let mut first_error = None;
            for worker in workers {
                match worker.join() {
                    Ok(Ok(worker_results)) => {
                        for (index, result) in worker_results {
                            results.insert(index, result);
                        }
                    }
                    Ok(Err(error)) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            first_error = Some(WorkspaceError::Io(std::io::Error::other(
                                "workspace source worker panicked",
                            )));
                        }
                    }
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            Ok(())
        })?;
        results.into_values().collect::<Vec<_>>()
    };

    for result in results {
        context.cancellation.checkpoint()?;
        merge_scan_report(report, result.report, context.limits);
        let Some(state) = result.state else {
            continue;
        };
        if let Some(existing) = files.insert(result.file.id, result.file.clone()) {
            return Err(WorkspaceError::FileIdCollision {
                first: existing.physical_path,
                second: result.file.physical_path,
            });
        }
        file_states.insert(result.file.id, state);
        report.indexed_files = report.indexed_files.saturating_add(1);
    }
    Ok(())
}

fn load_source_file_job(
    job: SourceReadJob,
    context: &SourceLoadContext<'_>,
) -> Result<SourceReadResult, WorkspaceError> {
    let mut report = WorkspaceScanReport::default();
    let text = read_source_file_cancellable(
        &job.physical_path,
        context.limits,
        &mut report,
        context.cancellation,
        context.profile.source_encoding,
    )?;
    let state = text.map(|text| {
        let previous = context.previous_states.get(&job.file.id);
        if let Some(previous) = previous
            && context.previous_files.get(&job.file.id) == Some(&job.file)
            && previous.source() == text
        {
            if job.retain_frontend || previous.parsed().is_none() {
                return Arc::clone(previous);
            }
            return Arc::new(
                previous.cache_only_from_existing(position_ranges_for_state(previous)),
            );
        }
        let file_revision = previous.map_or(0, |state| state.revision().saturating_add(1));
        let state = build_file_state(
            &job.file,
            text,
            file_revision,
            context.rules,
            context.profile,
        );
        if job.retain_frontend {
            Arc::new(state)
        } else {
            let positions = position_ranges_for_state(&state);
            Arc::new(state.cache_only(positions))
        }
    });
    Ok(SourceReadResult {
        file: job.file,
        state,
        report,
    })
}

fn merge_scan_report(
    report: &mut WorkspaceScanReport,
    partial: WorkspaceScanReport,
    limits: WorkspaceScanLimits,
) {
    report.skipped_entries = report
        .skipped_entries
        .saturating_add(partial.skipped_entries);
    report.legacy_encoded_files = report
        .legacy_encoded_files
        .saturating_add(partial.legacy_encoded_files);
    report.omitted_issues = report.omitted_issues.saturating_add(partial.omitted_issues);
    for issue in partial.issues {
        if report.issues.len() < limits.max_reported_issues {
            report.issues.push(issue);
        } else {
            report.omitted_issues = report.omitted_issues.saturating_add(1);
        }
    }
}

fn build_file_state(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
) -> FileState {
    let Some(category) = rules.classify(&file.logical_path) else {
        return FileState {
            revision,
            source: Arc::from(source),
            parsed: None,
            hir: None,
            shard: Arc::new(FileIndexShard {
                file_id: file.id,
                definitions: Vec::new(),
                references: Vec::new(),
                syntax_error_count: 0,
            }),
            cached_positions: None,
            cached_localisation_previews: None,
        };
    };
    let (parsed, hir) = parse_source(
        &category.parser,
        &source,
        Some(&file.logical_path),
        rules,
        profile,
    );
    let mut shard = match (parsed.as_ref(), hir.as_deref()) {
        (Some(ParsedSource::Text(parsed)), Some(hir)) => {
            shard_from_parsed(file, parsed, hir, rules, profile)
        }
        (Some(ParsedSource::Text(parsed)), None) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: parsed.errors().len(),
        },
        (None, _) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: 0,
        },
    };
    let mut seen_definitions = BTreeSet::new();
    shard.definitions.retain(|definition| {
        seen_definitions.insert((
            definition.kind.clone(),
            definition.name.clone(),
            definition.file_id,
            definition.range,
        ))
    });
    FileState {
        revision,
        source: Arc::from(source),
        parsed,
        hir,
        shard: Arc::new(shard),
        cached_positions: None,
        cached_localisation_previews: None,
    }
}

fn empty_file_state(file: &SourceFile, revision: u64) -> FileState {
    FileState {
        revision,
        source: Arc::from(""),
        parsed: None,
        hir: None,
        shard: Arc::new(FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: 0,
        }),
        cached_positions: None,
        cached_localisation_previews: None,
    }
}

fn position_ranges_for_state(state: &FileState) -> Vec<(TextRange, PositionRange)> {
    if let Some(cached) = state.cached_positions.as_deref() {
        return cached.clone();
    }
    let line_index = LineIndex::new(state.source());
    let hir_selection_ranges = state
        .hir()
        .map(|hir| {
            hir.definitions()
                .iter()
                .map(|definition| {
                    (
                        (
                            definition.kind.clone(),
                            definition.name.clone(),
                            definition.range,
                        ),
                        definition.selection_range,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    state
        .shard()
        .definitions
        .iter()
        .filter_map(|definition| {
            let selection_range = hir_selection_ranges
                .get(&(
                    definition.kind.clone(),
                    definition.name.clone(),
                    definition.range,
                ))
                .copied()
                .unwrap_or(definition.range);
            line_index
                .position_range(state.source(), selection_range)
                .map(|position| (definition.range, position))
        })
        .chain(state.shard().references.iter().filter_map(|reference| {
            line_index
                .position_range(state.source(), reference.range)
                .map(|position| (reference.range, position))
        }))
        .collect()
}

fn shard_from_parsed(
    file: &SourceFile,
    parsed: &ParsedFile,
    hir: &HirFile,
    rules: &RuleSet,
    profile: &GameProfile,
) -> FileIndexShard {
    let mut definitions = Vec::new();
    let mut references = Vec::new();
    collect_hir_semantics(file, hir, &mut definitions, &mut references);
    collect_profile_token_definitions(file, hir, profile, &mut definitions);
    collect_semantic_type_members(file, parsed, rules, &mut definitions);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        syntax_error_count: parsed.errors().len(),
    }
}

/// Collects workspace members declared by semantic `type[...]` definitions.
///
/// The semantic engine builds these members from the parsed workspace rather than treating a type's name as
/// a literal root key. For example, `type[mission]` with `skip_root_key = any` exposes every child
/// of every root clause in `missions/*.txt` as a `<mission>` member. Keeping this in the workspace
/// shard makes semantic key/value matching, completion, and hover see the same dynamic names.
fn collect_semantic_type_members(
    file: &SourceFile,
    parsed: &ParsedFile,
    rules: &RuleSet,
    definitions: &mut Vec<Definition>,
) {
    for descriptor in rules.model().semantic.type_descriptors.values() {
        if !semantic_type_path_matches(descriptor, &file.logical_path) {
            continue;
        }

        if descriptor.type_per_file {
            let Some(file_name) = file.logical_path.as_str().rsplit('/').next() else {
                continue;
            };
            let name = file_name
                .rsplit_once('.')
                .map_or(file_name, |(stem, _)| stem);
            if !name.is_empty() {
                definitions.push(Definition {
                    kind: descriptor.name.clone(),
                    name: name.to_owned(),
                    file_id: file.id,
                    range: parsed.root().range(),
                    active: true,
                });
            }
            continue;
        }

        if descriptor.skip_root_paths.is_empty() {
            for child in parsed.root().children() {
                if child.kind() == CstKind::Property {
                    collect_semantic_type_definition(file, parsed, descriptor, child, definitions);
                }
            }
        } else {
            for root in parsed.root().children() {
                if root.kind() != CstKind::Property {
                    continue;
                }
                for skip_path in &descriptor.skip_root_paths {
                    collect_semantic_skip_root_path(
                        file,
                        parsed,
                        descriptor,
                        root,
                        skip_path,
                        definitions,
                    );
                }
            }
        }
    }
}

fn collect_semantic_skip_root_path(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    path: &[String],
    definitions: &mut Vec<Definition>,
) {
    let Some(head) = path.first() else {
        collect_semantic_block_children(file, parsed, descriptor, node, definitions);
        return;
    };
    let node_key = semantic_property_key(node, parsed).unwrap_or_default();
    if !head.eq_ignore_ascii_case("any") && !head.eq_ignore_ascii_case(&node_key) {
        return;
    }
    if path.len() == 1 {
        collect_semantic_block_children(file, parsed, descriptor, node, definitions);
        return;
    }
    for child in semantic_block_properties(node) {
        collect_semantic_skip_root_path(file, parsed, descriptor, child, &path[1..], definitions);
    }
}

fn collect_semantic_block_children(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    for child in semantic_block_properties(node) {
        collect_semantic_type_definition(file, parsed, descriptor, child, definitions);
    }
}

fn collect_semantic_type_definition(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    let Some(key) = semantic_property_key(node, parsed) else {
        return;
    };
    if !semantic_type_key_matches(descriptor, &key) {
        return;
    }
    let Some(name) = descriptor
        .name_field
        .as_deref()
        .and_then(|field| find_property(node, field, parsed))
        .or(Some(key))
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    definitions.push(Definition {
        kind: descriptor.name.clone(),
        name,
        file_id: file.id,
        range: node.range(),
        active: true,
    });
}

fn semantic_type_key_matches(descriptor: &pdx_rules::TypeDescriptor, key: &str) -> bool {
    descriptor
        .type_key_filter
        .as_ref()
        .is_none_or(|(values, negate)| {
            (values.iter().any(|value| value.eq_ignore_ascii_case(key))) != *negate
        })
}

fn semantic_block_properties(node: &CstNode) -> impl Iterator<Item = &CstNode> {
    node.children().iter().flat_map(|child| {
        if child.kind() != CstKind::Value {
            return Vec::new();
        }
        child
            .children()
            .iter()
            .filter(|block| block.kind() == CstKind::Block)
            .flat_map(|block| {
                block
                    .children()
                    .iter()
                    .filter(|child| child.kind() == CstKind::Property)
            })
            .collect::<Vec<_>>()
    })
}

fn semantic_property_key(node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    node.children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)
        .and_then(|child| parsed.text(child.range()))
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn semantic_type_path_matches(
    descriptor: &pdx_rules::TypeDescriptor,
    logical_path: &LogicalPath,
) -> bool {
    let path = logical_path
        .as_str()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let (directory, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix = prefix
            .trim_matches('/')
            .strip_prefix("game/")
            .unwrap_or(prefix.trim_matches('/'));
        let prefix = prefix.to_ascii_lowercase();
        let matches = if descriptor.path_strict {
            directory == prefix
        } else {
            directory == prefix || directory.starts_with(&format!("{prefix}/"))
        };
        if !matches {
            return false;
        }
    }
    if let Some(expected_file) = descriptor.path_file.as_deref()
        && !file_name.eq_ignore_ascii_case(expected_file)
    {
        return false;
    }
    if let Some(expected_extension) = descriptor.path_extension.as_deref() {
        let expected_extension = expected_extension.trim_start_matches('.');
        let actual_extension = file_name
            .rsplit_once('.')
            .map_or("", |(_, extension)| extension);
        if !actual_extension.eq_ignore_ascii_case(expected_extension) {
            return false;
        }
    }
    true
}

fn collect_profile_token_definitions(
    file: &SourceFile,
    hir: &HirFile,
    profile: &GameProfile,
    definitions: &mut Vec<Definition>,
) {
    for rule in profile
        .token_definitions
        .iter()
        .filter(|rule| rule.path.matches(file.logical_path.as_str()))
    {
        for parameter in hir
            .parameter_definitions()
            .iter()
            .filter(|item| item.delimiter == rule.delimiter)
        {
            definitions.push(Definition {
                kind: rule.inner_kind.clone(),
                name: parameter.name.clone(),
                file_id: file.id,
                range: parameter.range,
                active: true,
            });
            definitions.push(Definition {
                kind: rule.wrapped_kind.clone(),
                name: format!("{}{}{}", rule.delimiter, parameter.name, rule.delimiter),
                file_id: file.id,
                range: parameter.range,
                active: true,
            });
        }
    }
}

fn collect_hir_semantics(
    file: &SourceFile,
    hir: &HirFile,
    definitions: &mut Vec<Definition>,
    references: &mut Vec<Reference>,
) {
    for definition in hir.definitions() {
        definitions.push(Definition {
            kind: definition.kind.clone(),
            name: definition.name.clone(),
            file_id: file.id,
            range: definition.range,
            active: true,
        });
    }
    for reference in hir.references() {
        references.push(Reference {
            kind: reference.kind.clone(),
            name: reference.name.clone(),
            file_id: file.id,
            range: reference.range,
        });
    }
}

fn find_property(node: &CstNode, wanted: &str, parsed: &ParsedFile) -> Option<String> {
    if node.kind() == CstKind::Property {
        let key = node
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Key)
            .and_then(|child| parsed.text(child.range()))
            .map(str::trim);
        if key == Some(wanted) {
            for child in node.children() {
                if matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString) {
                    return parsed
                        .text(child.range())
                        .map(|value| value.trim_matches('"').trim().to_owned());
                }
                if child.kind() == CstKind::Value
                    && let Some(value) = child.children().iter().find(|value| {
                        matches!(value.kind(), CstKind::BareValue | CstKind::QuotedString)
                    })
                {
                    return parsed
                        .text(value.range())
                        .map(|value| value.trim_matches('"').trim().to_owned());
                }
            }
        }
    }
    node.children()
        .iter()
        .find_map(|child| find_property(child, wanted, parsed))
}

/// Mutable owner of workspace state.
#[derive(Clone, Debug)]
pub struct AnalysisHost {
    revision: u64,
    rules: Arc<RuleSet>,
    profile: Arc<GameProfile>,
    roots: Arc<[SourceRoot]>,
    workspace_root: Option<PathBuf>,
    documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    index: Arc<WorkspaceIndex>,
    scan_report: Arc<WorkspaceScanReport>,
    vanilla_root: Option<SourceRoot>,
    vanilla_localisation_previews: Arc<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>>,
}

impl AnalysisHost {
    /// Creates an empty host with no game-specific rule identity.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(RuleSet::empty())
    }

    /// Creates an empty host around an immutable rule database.
    #[must_use]
    pub fn new(rules: RuleSet) -> Self {
        let profile = GameProfile::empty(rules.game_id());
        Self::with_profile(rules, profile)
    }

    /// Creates an empty host with explicit game-specific profile data.
    #[must_use]
    pub fn with_profile(rules: RuleSet, profile: GameProfile) -> Self {
        Self {
            revision: 0,
            rules: Arc::new(rules),
            profile: Arc::new(profile),
            roots: Arc::from([]),
            workspace_root: None,
            documents: Arc::new(BTreeMap::new()),
            source_files: Arc::new(BTreeMap::new()),
            file_states: Arc::new(BTreeMap::new()),
            index: Arc::new(WorkspaceIndex::empty()),
            scan_report: Arc::new(WorkspaceScanReport::default()),
            vanilla_root: None,
            vanilla_localisation_previews: Arc::new(BTreeMap::new()),
        }
    }

    fn document_snapshot(
        &self,
        id: DocumentId,
        version: Option<i64>,
        text: String,
        source: DocumentSource,
        path: Option<PathBuf>,
    ) -> DocumentSnapshot {
        prepare_document_snapshot(
            self.rules.as_ref(),
            self.profile.as_ref(),
            &self.roots,
            unparsed_document(id, version, text, source, path),
        )
    }

    /// Applies one event-loop change and advances the snapshot revision.
    pub fn apply_change(&mut self, change: WorkspaceChange) {
        match change {
            WorkspaceChange::SetSourceRoots(roots) => {
                self.roots = Arc::from(roots);
                self.vanilla_root = None;
                self.vanilla_localisation_previews = Arc::new(BTreeMap::new());
            }
            WorkspaceChange::SetWorkspaceRoot(root) => self.workspace_root = root,
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Installs a validated persistent Vanilla cache without scanning its original directory.
    ///
    /// The cache's rule hash is intentionally not required to match. It remains observable in
    /// metadata so the user can decide whether to run an explicit refresh.
    pub fn install_vanilla_cache(
        &mut self,
        cache: VanillaIndexCache,
    ) -> Result<(), VanillaCacheError> {
        if cache.metadata().game_id != self.rules.game_id()
            || cache.metadata().game_id != self.profile.game_id
        {
            return Err(VanillaCacheError::GameMismatch {
                expected: self.profile.game_id.clone(),
                actual: cache.metadata().game_id.clone(),
            });
        }
        let vanilla = cache.source_root();
        for root in self.roots.iter() {
            if root.id == vanilla.id {
                return Err(VanillaCacheError::InvalidData(format!(
                    "reserved Vanilla root id {} is already configured",
                    vanilla.id.get()
                )));
            }
            if root.kind == SourceRootKind::Vanilla {
                return Err(VanillaCacheError::InvalidData(
                    "a Vanilla source root is already configured".to_owned(),
                ));
            }
            if vanilla.path.starts_with(&root.path) || root.path.starts_with(&vanilla.path) {
                return Err(VanillaCacheError::RootConflict {
                    vanilla: vanilla.path.clone(),
                    configured: root.path.clone(),
                });
            }
        }
        if let Some(file) = cache
            .source_files()
            .values()
            .find(|file| !self.profile.allows_scan_file(file.logical_path.as_str()))
        {
            return Err(VanillaCacheError::InvalidData(format!(
                "Vanilla cache file {} is outside the active profile scan whitelist",
                file.logical_path.as_str()
            )));
        }

        let (_, vanilla, mut files, cached_index, cached_positions, cached_previews) =
            cache.into_parts();
        for (id, file) in self.source_files.iter() {
            if let Some(cached) = files.insert(*id, file.clone()) {
                return Err(VanillaCacheError::InvalidData(format!(
                    "file id collision between {} and {}",
                    cached.physical_path.display(),
                    file.physical_path.display()
                )));
            }
        }
        let mut shards = cached_index.shards.into_values().collect::<Vec<_>>();
        shards.extend(self.index.shards.values().cloned());
        let mut roots = Vec::with_capacity(self.roots.len().saturating_add(1));
        roots.push(vanilla.clone());
        roots.extend(self.roots.iter().cloned());
        let mut index = WorkspaceIndex::from_shards(shards);
        index.replace_all_position_ranges(cached_positions);
        for (file_id, state) in self.file_states.iter() {
            index.replace_position_ranges(*file_id, position_ranges_for_state(state));
        }
        index.configure_case_sensitivity(self.rules.as_ref());
        let priorities = source_priorities(&roots, &files);
        index.resolve_priorities(&priorities, self.rules.as_ref());

        self.roots = Arc::from(roots);
        self.source_files = Arc::new(files);
        self.index = Arc::new(index);
        self.vanilla_root = Some(vanilla);
        self.vanilla_localisation_previews = Arc::new(cached_previews);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Scans all configured roots in stable order and atomically refreshes source files and shards.
    pub fn refresh_source_roots(&mut self) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits(WorkspaceScanLimits::default())
    }

    /// Scans configured roots while cooperatively observing `cancellation`.
    pub fn refresh_source_roots_cancellable(
        &mut self,
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            WorkspaceScanLimits::default(),
            cancellation,
            None,
        )
    }

    /// Scans configured roots while cooperatively observing `cancellation`, invoking `progress`
    /// with `(completed, total)` source files so long-running background work can be surfaced.
    pub fn refresh_source_roots_cancellable_with_progress(
        &mut self,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            WorkspaceScanLimits::default(),
            cancellation,
            progress,
        )
    }

    /// Scans all configured roots with explicit resource limits.
    pub fn refresh_source_roots_with_limits(
        &mut self,
        limits: WorkspaceScanLimits,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            limits,
            &WorkspaceScanToken::new(),
            None,
        )
    }

    /// Scans configured roots with explicit resource limits and cooperative cancellation.
    pub fn refresh_source_roots_with_limits_and_cancellation(
        &mut self,
        limits: WorkspaceScanLimits,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        cancellation.checkpoint()?;
        let mut files: BTreeMap<SourceFileId, SourceFile> = BTreeMap::new();
        let mut file_states: BTreeMap<SourceFileId, Arc<FileState>> = BTreeMap::new();
        let mut source_jobs = Vec::new();
        let mut report = WorkspaceScanReport::default();
        for root in self.roots.iter() {
            cancellation.checkpoint()?;
            if self
                .vanilla_root
                .as_ref()
                .is_some_and(|vanilla| vanilla.id == root.id)
            {
                continue;
            }
            let mut paths = Vec::new();
            collect_whitelisted_files(
                &root.path,
                self.profile.as_ref(),
                limits,
                &mut report,
                &mut paths,
                cancellation,
            )?;
            paths.sort_by(|left, right| left.0.cmp(&right.0));
            for (logical, physical) in paths {
                cancellation.checkpoint()?;
                let id = SourceFileId::new(stable_file_id(root.id, &logical));
                let Some(category) = self.rules.classify(&logical) else {
                    continue;
                };
                let source_file = SourceFile {
                    id,
                    root_id: root.id,
                    physical_path: physical.clone(),
                    logical_path: logical,
                    category_id: Some(category.id.clone()),
                    resolution: category.resolution,
                };
                // Opaque resources participate in path/overlay resolution, but have no
                // text parser or semantic state. In particular, do not read binary assets as
                // UTF-8 just to manufacture an empty shard for them.
                if matches!(&category.parser, ParserKind::Asset) {
                    let file_for_state = source_file.clone();
                    if let Some(existing) = files.insert(id, source_file) {
                        return Err(WorkspaceError::FileIdCollision {
                            first: existing.physical_path,
                            second: physical,
                        });
                    }
                    let file_revision = self
                        .file_states
                        .get(&id)
                        .map_or(0, |state| state.revision().saturating_add(1));
                    let state = match self.file_states.get(&id) {
                        Some(previous)
                            if self.source_files.get(&id) == Some(&file_for_state)
                                && previous.source().is_empty() =>
                        {
                            Arc::clone(previous)
                        }
                        _ => Arc::new(empty_file_state(&file_for_state, file_revision)),
                    };
                    file_states.insert(id, state);
                    report.indexed_files = report.indexed_files.saturating_add(1);
                    continue;
                }
                source_jobs.push(SourceReadJob {
                    file: source_file,
                    physical_path: physical,
                    retain_frontend: root.kind != SourceRootKind::Vanilla,
                });
            }
        }
        let source_context = SourceLoadContext {
            limits,
            previous_files: self.source_files.as_ref(),
            previous_states: self.file_states.as_ref(),
            rules: self.rules.as_ref(),
            profile: self.profile.as_ref(),
            cancellation,
            progress,
        };
        load_source_files(
            source_jobs,
            &mut files,
            &mut file_states,
            &mut report,
            &source_context,
        )?;
        cancellation.checkpoint()?;
        let mut shards = file_states
            .values()
            .map(|state| state.shard().clone())
            .collect::<Vec<_>>();
        if let Some(vanilla) = self.vanilla_root.as_ref() {
            for (id, cached) in self
                .source_files
                .iter()
                .filter(|(_, file)| file.root_id == vanilla.id)
            {
                if let Some(existing) = files.insert(*id, cached.clone()) {
                    return Err(WorkspaceError::FileIdCollision {
                        first: existing.physical_path,
                        second: cached.physical_path.clone(),
                    });
                }
            }
            shards.extend(
                self.index
                    .shards
                    .iter()
                    .filter(|(id, _)| {
                        self.source_files
                            .get(id)
                            .is_some_and(|file| file.root_id == vanilla.id)
                    })
                    .map(|(_, shard)| shard.clone()),
            );
        }
        let mut index = WorkspaceIndex::from_shards_cancellable_with_rules(
            shards,
            self.rules.as_ref(),
            cancellation,
        )?;
        let mut position_ranges = self.index.position_ranges().clone();
        position_ranges.retain(|(file_id, _), _| {
            files.contains_key(file_id) && !file_states.contains_key(file_id)
        });
        for (file_id, state) in &file_states {
            position_ranges.extend(
                position_ranges_for_state(state)
                    .into_iter()
                    .map(|(range, position)| ((*file_id, range), position)),
            );
        }
        index.replace_all_position_ranges(position_ranges);
        let priorities = source_priorities(&self.roots, &files);
        let has_multiple_source_roots = self
            .roots
            .iter()
            .filter(|root| files.values().any(|file| file.root_id == root.id))
            .nth(1)
            .is_some();
        if self.vanilla_root.is_some() || has_multiple_source_roots {
            index.resolve_priorities_cancellable(&priorities, self.rules.as_ref(), cancellation)?;
        }
        cancellation.checkpoint()?;
        self.source_files = Arc::new(files);
        self.file_states = Arc::new(file_states);
        self.index = Arc::new(index);
        self.scan_report = Arc::new(report.clone());
        self.revision = self.revision.saturating_add(1);
        Ok(report)
    }

    /// Applies a batch of Current Mod/Dependency disk events with one atomic snapshot commit.
    ///
    /// Only changed file states and their index shards are rebuilt. Open overlays remain intact,
    /// and a persistent Vanilla root is never read or watched through this path.
    pub fn apply_disk_file_changes(
        &mut self,
        changes: &[DiskFileChange],
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.apply_disk_file_changes_cancellable(changes, &WorkspaceScanToken::new())
    }

    /// Applies targeted disk events while cooperatively observing `cancellation`.
    pub fn apply_disk_file_changes_cancellable(
        &mut self,
        changes: &[DiskFileChange],
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        cancellation.checkpoint()?;
        let limits = WorkspaceScanLimits::default();
        let mut files = self.source_files.as_ref().clone();
        let mut file_states = self.file_states.as_ref().clone();
        let mut index = self.index.as_ref().clone();
        let mut report = WorkspaceScanReport::default();
        let mut changed = false;

        for change in changes {
            cancellation.checkpoint()?;
            let Some(root) = self
                .roots
                .iter()
                .filter(|root| {
                    matches!(
                        root.kind,
                        SourceRootKind::CurrentMod | SourceRootKind::Dependency
                    )
                })
                .filter(|root| change.path.starts_with(&root.path))
                .max_by_key(|root| root.path.as_os_str().len())
            else {
                continue;
            };
            let relative = change
                .path
                .strip_prefix(&root.path)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(change.path.clone()))?
                .to_string_lossy()
                .replace('\\', "/");
            let logical = LogicalPath::parse(&relative)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(change.path.clone()))?;
            if !self.profile.allows_scan_file(logical.as_str()) {
                continue;
            }
            let id = SourceFileId::new(stable_file_id(root.id, &logical));
            report.discovered_files = report.discovered_files.saturating_add(1);

            let missing = match fs::symlink_metadata(&change.path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    record_scan_issue(
                        &mut report,
                        limits,
                        WorkspaceScanIssueKind::SymlinkSkipped,
                        change.path.clone(),
                        "symbolic links are not followed during targeted disk updates".to_owned(),
                    );
                    continue;
                }
                Ok(metadata) => !metadata.is_file(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    record_scan_issue(
                        &mut report,
                        limits,
                        WorkspaceScanIssueKind::MetadataUnreadable,
                        change.path.clone(),
                        error.to_string(),
                    );
                    continue;
                }
            };
            if change.kind == DiskFileChangeKind::Deleted || missing {
                if files.remove(&id).is_some() {
                    file_states.remove(&id);
                    let priorities = source_priorities(&self.roots, &files);
                    index.remove_shard_resolved(id, &priorities, self.rules.as_ref());
                    index.remove_position_ranges(id);
                    changed = true;
                }
                continue;
            }

            let Some(category) = self.rules.classify(&logical) else {
                continue;
            };
            let source_file = SourceFile {
                id,
                root_id: root.id,
                physical_path: change.path.clone(),
                logical_path: logical,
                category_id: Some(category.id.clone()),
                resolution: category.resolution,
            };
            if let Some(existing) = files.get(&id)
                && existing.physical_path != source_file.physical_path
            {
                return Err(WorkspaceError::FileIdCollision {
                    first: existing.physical_path.clone(),
                    second: source_file.physical_path,
                });
            }
            let text = if matches!(&category.parser, ParserKind::Asset) {
                None
            } else {
                let Some(text) = read_source_file_cancellable(
                    &change.path,
                    limits,
                    &mut report,
                    cancellation,
                    self.profile.source_encoding,
                )?
                else {
                    continue;
                };
                Some(text)
            };
            if files.get(&id) == Some(&source_file)
                && file_states
                    .get(&id)
                    .is_some_and(|state| text.as_deref().is_none_or(|text| state.source() == text))
            {
                continue;
            }
            let file_revision = file_states
                .get(&id)
                .map_or(0, |state| state.revision().saturating_add(1));
            let state = Arc::new(match text {
                Some(text) => build_file_state(
                    &source_file,
                    text,
                    file_revision,
                    self.rules.as_ref(),
                    self.profile.as_ref(),
                ),
                None => empty_file_state(&source_file, file_revision),
            });
            files.insert(id, source_file);
            file_states.insert(id, Arc::clone(&state));
            let priorities = source_priorities(&self.roots, &files);
            index.replace_shard_resolved(state.shard().clone(), &priorities, self.rules.as_ref());
            index.replace_position_ranges(id, position_ranges_for_state(&state));
            report.indexed_files = report.indexed_files.saturating_add(1);
            changed = true;
        }

        cancellation.checkpoint()?;
        if changed {
            self.source_files = Arc::new(files);
            self.file_states = Arc::new(file_states);
            self.index = Arc::new(index);
            self.scan_report = Arc::new(report.clone());
            self.revision = self.revision.saturating_add(1);
        }
        Ok(report)
    }

    /// Returns a mutable workspace index for targeted shard replacement.
    pub fn replace_index_shard(&mut self, shard: FileIndexShard) {
        let file_id = shard.file_id;
        let priorities = source_priorities(&self.roots, &self.source_files);
        Arc::make_mut(&mut self.index).replace_shard_resolved(
            shard.clone(),
            &priorities,
            self.rules.as_ref(),
        );
        Arc::make_mut(&mut self.index).remove_position_ranges(file_id);
        if let Some(previous) = self.file_states.get(&file_id) {
            let mut replacement = previous.as_ref().clone();
            replacement.shard = Arc::new(shard);
            Arc::make_mut(&mut self.index)
                .replace_position_ranges(file_id, position_ranges_for_state(&replacement));
            Arc::make_mut(&mut self.file_states).insert(file_id, Arc::new(replacement));
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Opens a document overlay with a complete initial text.
    pub fn open_document(
        &mut self,
        id: DocumentId,
        version: i64,
        text: String,
        path: Option<PathBuf>,
    ) -> Result<(), DocumentError> {
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.source == DocumentSource::Overlay)
        {
            return Err(DocumentError::AlreadyOpen(id));
        }
        let document = self.document_snapshot(
            id.clone(),
            Some(version),
            text,
            DocumentSource::Overlay,
            path,
        );
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Stages the latest overlay text without parsing it.
    ///
    /// A worker can call [`AnalysisSnapshot::prepare_document`] on the resulting snapshot and
    /// return the candidate to [`Self::commit_prepared_document`].
    pub fn stage_open_document(
        &mut self,
        id: DocumentId,
        version: i64,
        text: String,
        path: Option<PathBuf>,
    ) -> Result<(), DocumentError> {
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.source == DocumentSource::Overlay)
        {
            return Err(DocumentError::AlreadyOpen(id));
        }
        let document = staged_overlay_document(id.clone(), version, text, path);
        Arc::make_mut(&mut self.documents).insert(id, document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Stages a complete newer overlay text without parsing it.
    pub fn stage_document_text(
        &mut self,
        id: &DocumentId,
        version: i64,
        text: String,
    ) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let current_version = current.version.unwrap_or(version);
        if version <= current_version {
            return Err(DocumentError::StaleVersion {
                document: id.clone(),
                current: current_version,
                received: version,
            });
        }
        let document = staged_overlay_document(id.clone(), version, text, current.path.clone());
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Commits a worker-prepared document only while its exact staged text/version is current.
    pub fn commit_prepared_document(&mut self, prepared: PreparedDocument) -> bool {
        let id = prepared.document.id.clone();
        let Some(current) = self.documents.get(&id) else {
            return false;
        };
        let matches_current = current.source == DocumentSource::Overlay
            && current.version == prepared.document.version
            && current.text == prepared.document.text
            && current.path == prepared.document.path;
        if !matches_current {
            return false;
        }
        Arc::make_mut(&mut self.documents).insert(id, prepared.document);
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Applies all changes from one `didChange` notification atomically.
    pub fn apply_document_changes(
        &mut self,
        id: &DocumentId,
        version: i64,
        changes: &[TextChange],
    ) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let current_version = current.version.unwrap_or(version);
        if version <= current_version {
            return Err(DocumentError::StaleVersion {
                document: id.clone(),
                current: current_version,
                received: version,
            });
        }

        let mut text = current.text().to_owned();
        for change in changes {
            if let Some(range) = change.range {
                let start = usize::try_from(range.start()).ok();
                let end = usize::try_from(range.end()).ok();
                let valid = start
                    .zip(end)
                    .is_some_and(|(start, end)| start <= end && text.get(start..end).is_some());
                if !valid {
                    return Err(DocumentError::InvalidRange {
                        document: id.clone(),
                        range,
                    });
                }
                if let (Some(start), Some(end)) = (start, end) {
                    text.replace_range(start..end, &change.text);
                }
            } else {
                text = change.text.clone();
            }
        }

        let path = current.path.clone();
        let document = self.document_snapshot(
            id.clone(),
            Some(version),
            text,
            DocumentSource::Overlay,
            path,
        );
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Closes an overlay and restores its current disk candidate when available.
    pub fn close_document(&mut self, id: &DocumentId) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let path = current.path.clone();
        Arc::make_mut(&mut self.documents).remove(id);
        if let Some(path) = path {
            let mut report = WorkspaceScanReport::default();
            if let Some(text) = read_source_file(
                &path,
                WorkspaceScanLimits::default(),
                &mut report,
                self.profile.source_encoding,
            ) {
                let document = self.document_snapshot(
                    id.clone(),
                    None,
                    text,
                    DocumentSource::Disk,
                    Some(path),
                );
                Arc::make_mut(&mut self.documents).insert(id.clone(), document);
            }
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Captures an immutable query view.
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        AnalysisSnapshot {
            revision: self.revision,
            rules: Arc::clone(&self.rules),
            profile: Arc::clone(&self.profile),
            roots: Arc::clone(&self.roots),
            workspace_root: self.workspace_root.clone(),
            documents: Arc::clone(&self.documents),
            source_files: Arc::clone(&self.source_files),
            file_states: Arc::clone(&self.file_states),
            index: Arc::clone(&self.index),
            scan_report: Arc::clone(&self.scan_report),
            vanilla_localisation_previews: Arc::clone(&self.vanilla_localisation_previews),
        }
    }
}

/// Immutable workspace view used by analysis queries.
#[derive(Clone, Debug)]
pub struct AnalysisSnapshot {
    revision: u64,
    rules: Arc<RuleSet>,
    profile: Arc<GameProfile>,
    roots: Arc<[SourceRoot]>,
    workspace_root: Option<PathBuf>,
    documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    index: Arc<WorkspaceIndex>,
    scan_report: Arc<WorkspaceScanReport>,
    vanilla_localisation_previews: Arc<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>>,
}

impl AnalysisSnapshot {
    /// Returns the monotonic revision captured by this snapshot.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the immutable game rules used for this snapshot.
    #[must_use]
    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// Returns the immutable game-specific interpretation selected for this snapshot.
    #[must_use]
    pub fn game_profile(&self) -> &GameProfile {
        &self.profile
    }

    /// Clones the shared game-profile handle without copying profile data.
    #[must_use]
    pub fn game_profile_handle(&self) -> Arc<GameProfile> {
        Arc::clone(&self.profile)
    }

    /// Returns source roots in configured order.
    #[must_use]
    pub fn source_roots(&self) -> &[SourceRoot] {
        &self.roots
    }

    /// Returns the explicit workspace root, if configured.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_root.as_deref()
    }

    /// Returns all current document candidates keyed by stable document identity.
    #[must_use]
    pub fn documents(&self) -> &BTreeMap<DocumentId, DocumentSnapshot> {
        &self.documents
    }

    /// Fully parses and lowers the exact staged overlay captured by this snapshot.
    #[must_use]
    pub fn prepare_document(&self, id: &DocumentId) -> Option<PreparedDocument> {
        let document = self.documents.get(id)?;
        if document.source != DocumentSource::Overlay {
            return None;
        }
        Some(PreparedDocument {
            document: prepare_document_snapshot(
                self.rules.as_ref(),
                self.profile.as_ref(),
                &self.roots,
                document.clone(),
            ),
        })
    }

    /// Returns one current document candidate.
    #[must_use]
    pub fn document(&self, id: &DocumentId) -> Option<&DocumentSnapshot> {
        self.documents.get(id)
    }

    /// Returns all discovered source files.
    #[must_use]
    pub fn source_files(&self) -> &BTreeMap<SourceFileId, SourceFile> {
        &self.source_files
    }

    /// Returns the immutable parse/HIR/index state for one scanned disk file.
    #[must_use]
    pub fn file_state(&self, file_id: SourceFileId) -> Option<&FileState> {
        self.file_states.get(&file_id).map(Arc::as_ref)
    }

    /// Returns the immutable file/symbol index.
    #[must_use]
    pub fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Returns the bounded report from the latest successful source-root scan.
    #[must_use]
    pub fn scan_report(&self) -> &WorkspaceScanReport {
        &self.scan_report
    }

    /// Resolves one logical path, retaining lower-priority candidates as shadowed entries.
    #[must_use]
    pub fn resolve(&self, logical_path: &LogicalPath) -> Vec<ResolvedCandidate> {
        let mut candidates = self
            .source_files
            .values()
            .filter(|file| &file.logical_path == logical_path)
            .map(|file| {
                let priority = self
                    .roots
                    .iter()
                    .find(|root| root.id == file.root_id)
                    .map_or(0, root_priority);
                ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: Some(file.id),
                    document_id: None,
                    priority,
                    resolution: Some(file.resolution),
                    active: false,
                }
            })
            .collect::<Vec<_>>();
        for document in self.documents.values() {
            let Some(path) = document.path() else {
                continue;
            };
            let Some(root) = self.roots.iter().find(|root| path.starts_with(&root.path)) else {
                continue;
            };
            let Ok(relative) = path
                .strip_prefix(&root.path)
                .map(|value| LogicalPath::parse(&value.to_string_lossy()))
            else {
                continue;
            };
            if relative.as_ref().is_ok_and(|value| value == logical_path) {
                candidates.push(ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: None,
                    document_id: Some(document.id().clone()),
                    priority: 20_000,
                    resolution: None,
                    active: false,
                });
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority));
        let overlay_present = candidates
            .iter()
            .any(|candidate| candidate.document_id.is_some());
        if overlay_present {
            if let Some(first) = candidates.first_mut() {
                first.active = true;
            }
        } else if candidates
            .first()
            .is_some_and(|candidate| candidate.resolution == Some(FileResolutionPolicy::Merge))
        {
            for candidate in &mut candidates {
                candidate.active = true;
            }
        } else if let Some(first) = candidates.first_mut() {
            first.active = true;
        }
        candidates
    }

    /// Returns the current text for a disk file, if it was scanned.
    #[must_use]
    pub fn source_text(&self, file_id: SourceFileId) -> Option<&str> {
        self.file_state(file_id).map(FileState::source)
    }

    /// Returns a cached Vanilla localisation preview without reading the Vanilla source file.
    #[must_use]
    pub fn vanilla_localisation_preview(
        &self,
        file_id: SourceFileId,
        range: TextRange,
    ) -> Option<&LocalisationPreview> {
        self.vanilla_localisation_previews.get(&(file_id, range))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;

    use super::{
        AnalysisHost, Definition, DiskFileChange, DiskFileChangeKind, DocumentId, DocumentSource,
        FileIndexShard, ParsedSource, Reference, SourceFileId, SourceRoot, SourceRootId,
        SourceRootKind, TextChange, VanillaCacheError, VanillaIndexCache, WorkspaceError,
        WorkspaceIndex, WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanToken,
        pipeline_counts, reset_pipeline_counts,
    };
    use pdx_rules::{RuleSet, RulesModel, SymbolDescriptor, SymbolResolutionPolicy};
    use pdx_text::{LogicalPath, TextRange};

    fn eu4_host() -> AnalysisHost {
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile())
    }

    #[test]
    fn bulk_index_build_retains_every_shard_and_definition() {
        let first_file = SourceFileId::new(1);
        let second_file = SourceFileId::new(2);
        let range = TextRange::new(0, 3).expect("range");
        let shards = [
            FileIndexShard {
                file_id: first_file,
                definitions: vec![Definition {
                    kind: "event".to_owned(),
                    name: "shared.1".to_owned(),
                    file_id: first_file,
                    range,
                    active: true,
                }],
                references: Vec::new(),
                syntax_error_count: 0,
            },
            FileIndexShard {
                file_id: second_file,
                definitions: vec![Definition {
                    kind: "event".to_owned(),
                    name: "shared.1".to_owned(),
                    file_id: second_file,
                    range,
                    active: true,
                }],
                references: Vec::new(),
                syntax_error_count: 0,
            },
        ];

        let index = WorkspaceIndex::from_shards(shards);

        assert!(index.shard(first_file).is_some());
        assert!(index.shard(second_file).is_some());
        assert_eq!(index.definitions("event", "SHARED.1").len(), 2);
    }

    #[test]
    fn parallel_file_state_materialization_is_deterministic() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-parallel-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        for index in 0..64 {
            fs::write(
                events.join(format!("event-{index:02}.txt")),
                format!("country_event = {{ id = parallel.{index} }}\n"),
            )
            .expect("event fixture");
        }

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("parallel scan");
        assert_eq!(report.indexed_files, 64);
        let first = host.snapshot();
        assert_eq!(first.index().definitions("event", "parallel.63").len(), 1);

        host.refresh_source_roots()
            .expect("unchanged parallel scan");
        assert_eq!(host.snapshot().index(), first.index());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn symbol_case_policy_controls_definition_lookup_identity() {
        let file_id = SourceFileId::new(9);
        let range = TextRange::new(0, 3).expect("range");
        let mut model = RulesModel {
            game_id: "test".to_owned(),
            ..RulesModel::default()
        };
        model.symbol_descriptors.push(SymbolDescriptor {
            kind_id: "case_sensitive_kind".to_owned(),
            resolution: SymbolResolutionPolicy::ReplaceBySymbol,
            case_sensitive: true,
        });
        let rules = RuleSet::from_model(model);
        let mut index = WorkspaceIndex::from_shards([FileIndexShard {
            file_id,
            definitions: vec![Definition {
                kind: "case_sensitive_kind".to_owned(),
                name: "MixedName".to_owned(),
                file_id,
                range,
                active: true,
            }],
            references: Vec::new(),
            syntax_error_count: 0,
        }]);
        index.configure_case_sensitivity(&rules);

        assert_eq!(
            index.definitions("case_sensitive_kind", "MixedName").len(),
            1
        );
        assert!(
            index
                .definitions("case_sensitive_kind", "mixedname")
                .is_empty()
        );
    }

    #[test]
    fn identity_only_host_does_not_leak_eu4_dynamic_symbols() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-generic-profile-{nonce}"));
        let cultures = root.join("common/cultures");
        let scripted_effects = root.join("common/scripted_effects");
        for directory in [&cultures, &scripted_effects] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        fs::write(
            cultures.join("cultures.txt"),
            "germanic = { set_country_flag = generic_flag }\n",
        )
        .expect("culture fixture");
        fs::write(
            scripted_effects.join("effects.txt"),
            "example = { value = $AMOUNT$ }\n",
        )
        .expect("scripted effect fixture");

        let mut host = AnalysisHost::new(pdx_game::eu4::bootstrap_rules());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("scan roots");
        let snapshot = host.snapshot();

        assert!(
            snapshot
                .index()
                .definitions("culture", "germanic")
                .is_empty()
        );
        assert!(
            snapshot
                .index()
                .definitions("country_flag", "generic_flag")
                .is_empty()
        );
        assert!(
            snapshot
                .index()
                .definitions("scripted_effect_param", "AMOUNT")
                .is_empty()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn shard_replacement_updates_only_its_definition_and_reference_buckets() {
        let first_file = SourceFileId::new(1);
        let second_file = SourceFileId::new(2);
        let range = TextRange::new(0, 3).expect("range");
        let definition = |file_id, name: &str| Definition {
            kind: "event".to_owned(),
            name: name.to_owned(),
            file_id,
            range,
            active: true,
        };
        let reference = |file_id, name: &str| Reference {
            kind: "event".to_owned(),
            name: name.to_owned(),
            file_id,
            range,
        };
        let mut index = WorkspaceIndex::from_shards([
            FileIndexShard {
                file_id: first_file,
                definitions: vec![definition(first_file, "old.1")],
                references: vec![reference(first_file, "old.1")],
                syntax_error_count: 0,
            },
            FileIndexShard {
                file_id: second_file,
                definitions: vec![definition(second_file, "untouched.1")],
                references: vec![reference(second_file, "untouched.1")],
                syntax_error_count: 0,
            },
        ]);

        index.replace_shard(FileIndexShard {
            file_id: first_file,
            definitions: vec![definition(first_file, "new.1")],
            references: vec![reference(first_file, "new.1")],
            syntax_error_count: 1,
        });

        assert!(index.definitions("event", "old.1").is_empty());
        assert_eq!(index.definitions("event", "new.1").len(), 1);
        assert_eq!(index.definitions("event", "untouched.1").len(), 1);
        assert_eq!(index.references(first_file)[0].name, "new.1");
        assert_eq!(index.references(second_file)[0].name, "untouched.1");
        assert_eq!(
            index
                .shard(first_file)
                .expect("replacement shard")
                .syntax_error_count,
            1
        );

        index.remove_shard(first_file);
        assert!(index.definitions("event", "new.1").is_empty());
        assert!(index.references(first_file).is_empty());
        assert_eq!(index.definitions("event", "untouched.1").len(), 1);
    }

    #[test]
    fn replacement_re_resolves_only_affected_symbol_buckets_without_hiding_ties() {
        let first_file = SourceFileId::new(1);
        let second_file = SourceFileId::new(2);
        let range = TextRange::new(0, 3).expect("range");
        let definition = |file_id| Definition {
            kind: "event".to_owned(),
            name: "shared.1".to_owned(),
            file_id,
            range,
            active: true,
        };
        let mut index = WorkspaceIndex::from_shards([
            FileIndexShard {
                file_id: first_file,
                definitions: vec![definition(first_file)],
                references: Vec::new(),
                syntax_error_count: 0,
            },
            FileIndexShard {
                file_id: second_file,
                definitions: vec![definition(second_file)],
                references: Vec::new(),
                syntax_error_count: 0,
            },
        ]);
        let rules = pdx_game::eu4::bootstrap_rules();
        let tied = BTreeMap::from([(first_file, 10), (second_file, 10)]);
        index.resolve_priorities(&tied, &rules);
        assert_eq!(
            index
                .definitions("event", "shared.1")
                .iter()
                .filter(|item| item.active)
                .count(),
            2
        );
        assert!(index.active_definition("event", "shared.1").is_none());

        let ordered = BTreeMap::from([(first_file, 10), (second_file, 20)]);
        index.resolve_priorities(&ordered, &rules);
        assert_eq!(
            index
                .active_definition("event", "shared.1")
                .expect("higher priority definition")
                .file_id,
            second_file
        );
        index.remove_shard_resolved(second_file, &ordered, &rules);
        assert_eq!(
            index
                .active_definition("event", "shared.1")
                .expect("remaining definition")
                .file_id,
            first_file
        );
    }

    #[test]
    fn identical_collector_records_resolve_as_one_physical_definition() {
        let file_id = SourceFileId::new(1);
        let range = TextRange::new(4, 12).expect("range");
        let definition = Definition {
            kind: "scripted_effect".to_owned(),
            name: "apply".to_owned(),
            file_id,
            range,
            active: true,
        };
        let index = WorkspaceIndex::from_shards([FileIndexShard {
            file_id,
            definitions: vec![definition.clone(), definition],
            references: Vec::new(),
            syntax_error_count: 0,
        }]);

        assert_eq!(
            index
                .active_definition("scripted_effect", "apply")
                .expect("identical records are one physical definition")
                .range,
            range
        );

        let distinct_range = TextRange::new(20, 28).expect("distinct range");
        let distinct = WorkspaceIndex::from_shards([FileIndexShard {
            file_id,
            definitions: vec![
                Definition {
                    kind: "scripted_effect".to_owned(),
                    name: "apply".to_owned(),
                    file_id,
                    range,
                    active: true,
                },
                Definition {
                    kind: "scripted_effect".to_owned(),
                    name: "apply".to_owned(),
                    file_id,
                    range: distinct_range,
                    active: true,
                },
            ],
            references: Vec::new(),
            syntax_error_count: 0,
        }]);
        assert!(
            distinct
                .active_definition("scripted_effect", "apply")
                .is_none()
        );
    }

    #[test]
    fn targeted_disk_changes_replace_one_shard_without_overwriting_an_overlay() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-targeted-disk-{nonce}"));
        let events = root.join("common/events");
        fs::create_dir_all(&events).expect("fixture directory");
        let changed_path = events.join("changed.txt");
        let untouched_path = events.join("untouched.txt");
        fs::write(&changed_path, "country_event = { id = old.1 }\n").expect("changed fixture");
        fs::write(&untouched_path, "country_event = { id = untouched.1 }\n")
            .expect("untouched fixture");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");
        let before = host.snapshot();
        let changed_id = before
            .source_files()
            .values()
            .find(|file| file.physical_path == changed_path)
            .expect("changed source")
            .id;
        let untouched_id = before
            .source_files()
            .values()
            .find(|file| file.physical_path == untouched_path)
            .expect("untouched source")
            .id;
        let untouched_state = Arc::clone(before.file_states.get(&untouched_id).expect("state"));
        let document = DocumentId::new("file:///targeted/changed.txt");
        host.open_document(
            document.clone(),
            1,
            "country_event = { id = overlay.1 }\n".to_owned(),
            Some(changed_path.clone()),
        )
        .expect("open overlay");

        fs::write(&changed_path, "country_event = { id = new.1 }\n").expect("disk edit");
        reset_pipeline_counts();
        host.apply_disk_file_changes(&[DiskFileChange::new(
            changed_path.clone(),
            DiskFileChangeKind::Changed,
        )])
        .expect("targeted change");
        assert_eq!(pipeline_counts(), (1, 1));
        let changed = host.snapshot();
        assert!(
            changed
                .index()
                .active_definition("event", "old.1")
                .is_none()
        );
        assert!(
            changed
                .index()
                .active_definition("event", "new.1")
                .is_some()
        );
        assert_eq!(
            changed.document(&document).expect("overlay remains").text(),
            "country_event = { id = overlay.1 }\n"
        );
        assert!(Arc::ptr_eq(
            changed
                .file_states
                .get(&untouched_id)
                .expect("untouched current state"),
            &untouched_state
        ));
        assert!(!Arc::ptr_eq(
            changed
                .file_states
                .get(&changed_id)
                .expect("changed current state"),
            before
                .file_states
                .get(&changed_id)
                .expect("changed old state")
        ));

        let created_path = events.join("created.txt");
        fs::write(&created_path, "country_event = { id = created.1 }\n").expect("created fixture");
        host.apply_disk_file_changes(&[DiskFileChange::new(
            created_path,
            DiskFileChangeKind::Created,
        )])
        .expect("targeted create");
        assert!(
            host.snapshot()
                .index()
                .active_definition("event", "created.1")
                .is_some()
        );

        fs::remove_file(&changed_path).expect("delete changed fixture");
        host.apply_disk_file_changes(&[DiskFileChange::new(
            changed_path,
            DiskFileChangeKind::Deleted,
        )])
        .expect("targeted delete");
        let deleted = host.snapshot();
        assert!(deleted.source_files().get(&changed_id).is_none());
        assert!(
            deleted
                .index()
                .active_definition("event", "new.1")
                .is_none()
        );
        assert_eq!(
            deleted
                .document(&document)
                .expect("overlay survives backing deletion")
                .text(),
            "country_event = { id = overlay.1 }\n"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn source_file_ids_do_not_shift_when_an_earlier_path_is_added() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-stable-ids-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("b.txt"), "country_event = { id = stable.b }\n").expect("b event");
        fs::write(events.join("c.txt"), "country_event = { id = stable.c }\n").expect("c event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");
        let before = host.snapshot();
        let b_before = before
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/b.txt")
            .expect("b source file")
            .id;
        let c_before = before
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/c.txt")
            .expect("c source file")
            .id;

        fs::write(events.join("a.txt"), "country_event = { id = stable.a }\n").expect("a event");
        host.refresh_source_roots().expect("second scan");
        let after = host.snapshot();
        let b_after = after
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/b.txt")
            .expect("b source file after insertion")
            .id;
        let c_after = after
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/c.txt")
            .expect("c source file after insertion")
            .id;

        assert_eq!(b_before, b_after);
        assert_eq!(c_before, c_after);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unchanged_file_states_are_reused_and_only_changed_files_advance() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-file-state-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("a.txt"), "country_event = { id = state.a }\n").expect("a event");
        fs::write(events.join("b.txt"), "country_event = { id = state.b }\n").expect("b event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");
        let first = host.snapshot();
        let a = first
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/a.txt")
            .expect("a file")
            .id;
        let b = first
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/b.txt")
            .expect("b file")
            .id;
        assert!(first.file_state(a).expect("a state").parsed().is_some());
        assert!(first.file_state(a).expect("a state").hir().is_some());
        let Some(ParsedSource::Text(parsed)) = first.file_state(a).expect("a state").parsed()
        else {
            panic!("event file should retain a text parse");
        };
        assert!(std::ptr::eq(
            parsed.as_ref(),
            first
                .file_state(a)
                .expect("a state")
                .hir()
                .expect("a HIR")
                .syntax()
        ));

        host.refresh_source_roots().expect("unchanged scan");
        let second = host.snapshot();
        assert!(Arc::ptr_eq(
            first.file_states.get(&a).expect("first a state"),
            second.file_states.get(&a).expect("second a state")
        ));
        assert!(Arc::ptr_eq(
            first.file_states.get(&b).expect("first b state"),
            second.file_states.get(&b).expect("second b state")
        ));
        let old_range = second
            .index()
            .active_definition("event", "state.b")
            .expect("old b definition")
            .range;
        assert!(second.index().position_for(b, old_range).is_some());

        fs::write(
            events.join("b.txt"),
            "country_event = { id = state.changed }\n",
        )
        .expect("changed b event");
        host.refresh_source_roots().expect("changed scan");
        let third = host.snapshot();
        assert!(Arc::ptr_eq(
            second.file_states.get(&a).expect("second a state"),
            third.file_states.get(&a).expect("third a state")
        ));
        assert!(!Arc::ptr_eq(
            second.file_states.get(&b).expect("second b state"),
            third.file_states.get(&b).expect("third b state")
        ));
        assert_eq!(
            third.file_state(b).expect("changed b state").revision(),
            second
                .file_state(b)
                .expect("old b state")
                .revision()
                .saturating_add(1)
        );
        assert_eq!(third.index().definitions("event", "state.changed").len(), 1);
        let new_range = third
            .index()
            .active_definition("event", "state.changed")
            .expect("new b definition")
            .range;
        assert!(third.index().position_for(b, old_range).is_none());
        assert!(third.index().position_for(b, new_range).is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn one_overlay_edit_parses_and_lowers_exactly_once_in_a_populated_workspace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-pipeline-count-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        for index in 0..64 {
            fs::write(
                events.join(format!("event-{index:02}.txt")),
                format!("country_event = {{ id = synthetic.{index} }}\n"),
            )
            .expect("event fixture");
        }

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");

        let path = events.join("event-00.txt");
        let id = DocumentId::new("file:///synthetic/events/event-00.txt");
        host.stage_open_document(
            id.clone(),
            1,
            "country_event = { id = synthetic.0 }\n".to_owned(),
            Some(path),
        )
        .expect("stage initial overlay");
        let initial = host
            .snapshot()
            .prepare_document(&id)
            .expect("prepare initial overlay");
        assert!(host.commit_prepared_document(initial));
        let before_edit = host.snapshot();

        reset_pipeline_counts();
        host.stage_document_text(
            &id,
            2,
            "country_event = { id = synthetic.changed }\n".to_owned(),
        )
        .expect("stage edit");
        assert_eq!(
            pipeline_counts(),
            (0, 0),
            "staging must not run semantic work"
        );

        let prepared = host
            .snapshot()
            .prepare_document(&id)
            .expect("prepare edited overlay");
        assert_eq!(pipeline_counts(), (1, 1));
        assert!(host.commit_prepared_document(prepared));
        assert_eq!(
            pipeline_counts(),
            (1, 1),
            "commit must not repeat worker work"
        );

        let after_edit = host.snapshot();
        for file_id in before_edit.source_files().keys() {
            assert!(Arc::ptr_eq(
                before_edit
                    .file_states
                    .get(file_id)
                    .expect("old disk state"),
                after_edit
                    .file_states
                    .get(file_id)
                    .expect("current disk state"),
            ));
        }
        assert!(
            after_edit
                .document(&id)
                .expect("edited overlay")
                .hir()
                .is_some_and(|hir| {
                    hir.definitions()
                        .iter()
                        .any(|definition| definition.name == "synthetic.changed")
                })
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn snapshots_share_immutable_state_and_preserve_old_revisions() {
        let mut host = AnalysisHost::new(RuleSet::empty());
        let first = host.snapshot();
        let second = host.snapshot();

        assert!(Arc::ptr_eq(&first.rules, &second.rules));
        assert!(Arc::ptr_eq(&first.profile, &second.profile));
        assert!(Arc::ptr_eq(&first.roots, &second.roots));
        assert!(Arc::ptr_eq(&first.documents, &second.documents));
        assert!(Arc::ptr_eq(&first.source_files, &second.source_files));
        assert!(Arc::ptr_eq(&first.file_states, &second.file_states));
        assert!(Arc::ptr_eq(&first.index, &second.index));
        assert!(Arc::ptr_eq(&first.scan_report, &second.scan_report));

        let id = DocumentId::new("file:///tmp/snapshot.txt");
        host.open_document(id.clone(), 1, "one".to_owned(), None)
            .expect("open should succeed");
        let third = host.snapshot();

        assert!(first.document(&id).is_none());
        assert_eq!(
            third
                .document(&id)
                .expect("new snapshot sees document")
                .text(),
            "one"
        );
        assert!(!Arc::ptr_eq(&first.documents, &third.documents));
        assert!(Arc::ptr_eq(&first.roots, &third.roots));
        assert!(Arc::ptr_eq(&first.profile, &third.profile));
        assert!(Arc::ptr_eq(&first.source_files, &third.source_files));
        assert!(Arc::ptr_eq(&first.file_states, &third.file_states));
        assert!(Arc::ptr_eq(&first.index, &third.index));
        assert!(Arc::ptr_eq(&first.scan_report, &third.scan_report));
    }

    #[test]
    fn recoverable_file_failures_do_not_abort_the_workspace_scan() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-isolation-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("good.txt"), "country_event = { id = safe.1 }\n")
            .expect("valid event");
        fs::write(events.join("invalid.txt"), [0xff, 0xfe]).expect("invalid UTF-8 event");
        fs::write(
            events.join("undefined-windows1252.txt"),
            b"country_event = { id = invalid.1 }\n# \x81\n",
        )
        .expect("invalid Windows-1252 event");
        fs::write(events.join("large.txt"), vec![b'x'; 65]).expect("oversized event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host
            .refresh_source_roots_with_limits(WorkspaceScanLimits {
                max_file_size: 64,
                ..WorkspaceScanLimits::default()
            })
            .expect("recoverable file failures should not abort scanning");

        assert_eq!(report.discovered_files, 4);
        assert_eq!(report.indexed_files, 1);
        assert_eq!(report.skipped_entries, 3);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == WorkspaceScanIssueKind::InvalidUtf8)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == WorkspaceScanIssueKind::FileTooLarge)
        );
        assert_eq!(host.snapshot().scan_report(), &report);
        assert_eq!(host.snapshot().source_files().len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_legacy_windows1252_text_is_decoded_before_indexing() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-windows1252-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(
            events.join("legacy.txt"),
            b"country_event = { id = legacy.1 }\n# caf\xe9\n",
        )
        .expect("legacy event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("legacy scan");

        assert_eq!(report.indexed_files, 1);
        assert_eq!(report.legacy_encoded_files, 1);
        assert_eq!(report.skipped_entries, 0);
        let file_id = host
            .snapshot()
            .source_files()
            .values()
            .find(|file| file.logical_path.as_str() == "events/legacy.txt")
            .expect("legacy source file")
            .id;
        assert_eq!(
            host.snapshot().source_text(file_id),
            Some("country_event = { id = legacy.1 }\n# café\n")
        );
        assert_eq!(
            host.snapshot()
                .index()
                .definitions("event", "legacy.1")
                .len(),
            1
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn game_encoded_text_with_control_characters_is_not_loaded() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-non-text-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(
            events.join("encoded.txt"),
            b"country_event = { id = encoded.1 }\n# \x0c\x02garbage\n",
        )
        .expect("game-encoded event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("game-encoded scan");

        assert_eq!(report.indexed_files, 0);
        assert_eq!(report.legacy_encoded_files, 0);
        assert_eq!(report.skipped_entries, 1);
        assert!(
            report.issues.iter().any(|issue| {
                issue.kind == super::WorkspaceScanIssueKind::NonTextContent
                    && issue.path.ends_with("events/encoded.txt")
            }),
            "expected a NonTextContent issue: {:?}",
            report.issues
        );
        assert_eq!(host.snapshot().source_files().len(), 0);
        assert!(
            host.snapshot()
                .index()
                .definitions("event", "encoded.1")
                .is_empty()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn depth_limit_skips_nested_subtrees_with_a_reported_issue() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-depth-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("deep.txt"), "country_event = { id = deep.1 }\n")
            .expect("deep event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host
            .refresh_source_roots_with_limits(WorkspaceScanLimits {
                max_depth: 0,
                ..WorkspaceScanLimits::default()
            })
            .expect("depth-limited scan");

        assert_eq!(report.indexed_files, 0);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == WorkspaceScanIssueKind::DepthLimitExceeded)
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn file_limit_failure_preserves_the_previous_snapshot() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-file-limit-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("a.txt"), "country_event = { id = limit.a }\n").expect("a event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");
        let before = host.snapshot();
        fs::write(events.join("b.txt"), "country_event = { id = limit.b }\n").expect("b event");

        let error = host
            .refresh_source_roots_with_limits(WorkspaceScanLimits {
                max_files: 1,
                ..WorkspaceScanLimits::default()
            })
            .expect_err("the total file limit must be enforced");
        assert!(matches!(
            error,
            super::WorkspaceError::FileLimitExceeded { limit: 1 }
        ));
        let after = host.snapshot();
        assert_eq!(after.revision(), before.revision());
        assert_eq!(after.source_files(), before.source_files());
        assert_eq!(after.scan_report(), before.scan_report());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cancelled_scan_preserves_the_previous_snapshot_atomically() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-cancel-scan-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(
            events.join("baseline.txt"),
            "country_event = { id = baseline.1 }\n",
        )
        .expect("baseline event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        host.refresh_source_roots().expect("initial scan");
        let before = host.snapshot();
        for index in 0..32 {
            fs::write(
                events.join(format!("new-{index:02}.txt")),
                format!("country_event = {{ id = cancelled.{index} }}\n"),
            )
            .expect("new event");
        }

        let cancellation = WorkspaceScanToken::cancel_after(5);
        let error = host
            .refresh_source_roots_cancellable(&cancellation)
            .expect_err("scan should stop at an internal checkpoint");
        assert!(matches!(error, WorkspaceError::Cancelled));
        assert!(cancellation.is_cancelled());

        let after = host.snapshot();
        assert_eq!(after.revision(), before.revision());
        assert!(Arc::ptr_eq(&after.source_files, &before.source_files));
        assert!(Arc::ptr_eq(&after.file_states, &before.file_states));
        assert!(Arc::ptr_eq(&after.index, &before.index));
        assert!(Arc::ptr_eq(&after.scan_report, &before.scan_report));
        assert!(after.index().definitions("event", "cancelled.0").is_empty());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn opaque_binary_assets_are_indexed_without_reading_them_as_utf8() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-opaque-asset-{nonce}"));
        fs::create_dir_all(root.join("gfx")).expect("asset directory");
        fs::write(root.join("gfx/icon.png"), [0_u8, 159, 146, 150]).expect("binary asset");

        let mut profile = pdx_game::eu4::profile();
        profile.scan_extensions.clear();
        let mut host = AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), profile);
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("scan asset");

        assert_eq!(report.discovered_files, 1);
        assert_eq!(report.indexed_files, 1);
        assert!(
            !report
                .issues
                .iter()
                .any(|issue| issue.kind == WorkspaceScanIssueKind::InvalidUtf8)
        );
        let snapshot = host.snapshot();
        let file = snapshot
            .source_files()
            .values()
            .next()
            .expect("asset source file");
        assert_eq!(file.logical_path.as_str(), "gfx/icon.png");
        assert!(snapshot.index().shard(file.id).is_some());
        assert!(snapshot.file_state(file.id).is_some_and(|state| {
            state.parsed().is_none() && state.shard().definitions.is_empty()
        }));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn eu4_scan_uses_the_cwtools_script_folder_whitelist() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-whitelist-{nonce}"));
        fs::create_dir_all(root.join("events")).expect("events directory");
        fs::create_dir_all(root.join("common/custom_unknown")).expect("common directory");
        fs::create_dir_all(root.join("gfx")).expect("gfx directory");
        fs::create_dir_all(root.join("ignored")).expect("ignored directory");
        fs::write(
            root.join("events/allowed.txt"),
            "country_event = { id = whitelist.event }\n",
        )
        .expect("event fixture");
        fs::write(
            root.join("common/custom_unknown/allowed.txt"),
            "country_event = { id = whitelist.common }\n",
        )
        .expect("common fixture");
        fs::write(
            root.join("gfx/icon.gfx"),
            "spriteType = { name = whitelist.gfx }\n",
        )
        .expect("gfx fixture");
        fs::write(root.join("gfx/icon.png"), [0_u8, 159, 146, 150]).expect("asset fixture");
        fs::write(
            root.join("ignored/not_scanned.txt"),
            "country_event = { id = whitelist.ignored }\n",
        )
        .expect("ignored fixture");
        fs::write(
            root.join("root_level.txt"),
            "country_event = { id = whitelist.root }\n",
        )
        .expect("root-level fixture");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("whitelist scan");
        assert_eq!(report.discovered_files, 4);
        assert_eq!(report.indexed_files, 3);
        let snapshot = host.snapshot();
        assert!(
            snapshot
                .index()
                .active_definition("event", "whitelist.event")
                .is_some()
        );
        assert!(
            snapshot
                .index()
                .active_definition("event", "whitelist.common")
                .is_some()
        );
        assert!(
            snapshot
                .index()
                .active_definition("event", "whitelist.ignored")
                .is_none()
        );
        assert!(
            snapshot
                .index()
                .active_definition("event", "whitelist.root")
                .is_none()
        );
        assert!(
            snapshot
                .source_files()
                .values()
                .any(|file| file.logical_path.as_str() == "gfx/icon.gfx")
        );
        assert!(
            snapshot
                .source_files()
                .values()
                .all(|file| file.logical_path.as_str() != "gfx/icon.png")
        );
        let ignored_change = root.join("ignored/created_after_scan.txt");
        fs::write(
            &ignored_change,
            "country_event = { id = whitelist.watched_ignored }\n",
        )
        .expect("ignored watched fixture");
        host.apply_disk_file_changes(&[DiskFileChange::new(
            ignored_change,
            DiskFileChangeKind::Created,
        )])
        .expect("ignored watched change");
        assert!(
            host.snapshot()
                .index()
                .active_definition("event", "whitelist.watched_ignored")
                .is_none()
        );
        let ignored_extension_change = root.join("events/created_after_scan.png");
        fs::write(&ignored_extension_change, [0_u8, 159, 146, 150])
            .expect("ignored extension fixture");
        host.apply_disk_file_changes(&[DiskFileChange::new(
            ignored_extension_change,
            DiskFileChangeKind::Created,
        )])
        .expect("ignored extension change");
        assert!(
            host.snapshot()
                .source_files()
                .values()
                .all(|file| { file.logical_path.as_str() != "events/created_after_scan.png" })
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_reported_and_never_followed() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-symlink-root-{nonce}"));
        let outside = std::env::temp_dir().join(format!("pdx-engine-symlink-outside-{nonce}"));
        fs::create_dir_all(&root).expect("source root");
        fs::create_dir_all(&outside).expect("outside directory");
        fs::write(
            outside.join("leak.txt"),
            "country_event = { id = leak.1 }\n",
        )
        .expect("outside event");
        symlink(&outside, root.join("events")).expect("directory symlink");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("symlink-safe scan");

        assert_eq!(report.discovered_files, 0);
        assert_eq!(report.indexed_files, 0);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == WorkspaceScanIssueKind::SymlinkSkipped)
        );
        fs::remove_dir_all(root).expect("cleanup root");
        fs::remove_dir_all(outside).expect("cleanup outside directory");
    }

    #[test]
    fn workspace_scan_skips_tool_generated_directories() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-ignored-tools-{nonce}"));
        let events = root.join("events");
        let generated_events = root.join("target/debug/events");
        fs::create_dir_all(&events).expect("events directory");
        fs::create_dir_all(&generated_events).expect("generated events directory");
        fs::write(
            events.join("indexed.txt"),
            "country_event = { id = indexed.1 }\n",
        )
        .expect("indexed fixture");
        fs::write(
            generated_events.join("ignored.txt"),
            "country_event = { id = ignored.1 }\n",
        )
        .expect("ignored fixture");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root.clone(),
            ),
        ]));
        let report = host.refresh_source_roots().expect("bounded workspace scan");

        assert_eq!(report.discovered_files, 1);
        assert_eq!(report.indexed_files, 1);
        assert!(
            host.snapshot()
                .index()
                .definitions("event", "indexed.1")
                .len()
                == 1
        );
        assert!(
            host.snapshot()
                .index()
                .definitions("event", "ignored.1")
                .is_empty()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stale_document_versions_are_rejected_atomically() {
        let mut host = AnalysisHost::new(RuleSet::empty());
        let id = DocumentId::new("file:///tmp/example.txt");
        host.open_document(id.clone(), 1, "a😀z".to_owned(), None)
            .expect("open should succeed");
        let first = host.snapshot();
        let first_document = first.document(&id).expect("first document");
        let Some(ParsedSource::Text(first_parse)) = first_document.parsed() else {
            panic!("txt overlay should retain a text parse");
        };
        assert!(std::ptr::eq(
            first_parse.as_ref(),
            first_document.hir().expect("overlay HIR").syntax()
        ));
        let range = TextRange::new(1, 5).expect("emoji range");
        let error = host
            .apply_document_changes(&id, 1, &[TextChange::ranged(range, "x")])
            .expect_err("same version must be rejected");
        assert!(matches!(error, super::DocumentError::StaleVersion { .. }));
        assert_eq!(
            host.snapshot()
                .document(&id)
                .expect("document exists")
                .text(),
            "a😀z"
        );
        host.apply_document_changes(&id, 2, &[TextChange::ranged(range, "x")])
            .expect("new version should succeed");
        let second = host.snapshot();
        let second_document = second.document(&id).expect("document exists");
        assert_eq!(second_document.text(), "axz");
        let Some(ParsedSource::Text(second_parse)) = second_document.parsed() else {
            panic!("changed txt overlay should retain a text parse");
        };
        assert!(!Arc::ptr_eq(first_parse, second_parse));
        assert_eq!(
            first
                .document(&id)
                .expect("old snapshot remains valid")
                .text(),
            "a😀z"
        );
    }

    #[test]
    fn prepared_document_commit_rejects_superseded_text_and_version() {
        let mut host = eu4_host();
        let id = DocumentId::new("file:///tmp/events/prepared.txt");
        host.stage_open_document(
            id.clone(),
            1,
            "country_event = { id = stale.1 }\n".to_owned(),
            None,
        )
        .expect("stage open");
        let staged = host.snapshot();
        assert!(
            staged
                .document(&id)
                .expect("staged document")
                .parsed()
                .is_none()
        );
        let stale = staged
            .prepare_document(&id)
            .expect("prepare stale candidate");

        host.stage_document_text(&id, 2, "country_event = { id = current.1 }\n".to_owned())
            .expect("stage newer text");
        assert!(!host.commit_prepared_document(stale));
        let current = host
            .snapshot()
            .prepare_document(&id)
            .expect("prepare current candidate");
        assert!(host.commit_prepared_document(current));

        let committed = host.snapshot();
        let document = committed.document(&id).expect("committed document");
        assert_eq!(document.version(), Some(2));
        assert!(document.parsed().is_some());
        assert!(document.hir().is_some_and(|hir| {
            hir.definitions()
                .iter()
                .any(|definition| definition.kind == "event" && definition.name == "current.1")
        }));
    }

    #[test]
    fn close_restores_the_backing_disk_candidate() {
        let path = std::env::temp_dir().join(format!("pdx-engine-{}.txt", std::process::id()));
        fs::write(&path, "disk").expect("write fixture");
        let mut host = AnalysisHost::new(RuleSet::empty());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                path.parent().expect("temp parent").to_owned(),
            ),
        ]));
        let id = DocumentId::new("file:///tmp/pdx-engine.txt");
        host.open_document(id.clone(), 1, "overlay".to_owned(), Some(path.clone()))
            .expect("open should succeed");
        host.close_document(&id).expect("close should succeed");
        let snapshot = host.snapshot();
        let document = snapshot.document(&id).expect("disk candidate exists");
        assert_eq!(document.source(), DocumentSource::Disk);
        assert_eq!(document.version(), None);
        assert_eq!(document.text(), "disk");
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn roots_overlay_and_shards_preserve_shadowed_semantic_definitions() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-phase4-{nonce}"));
        let vanilla = root.join("vanilla");
        let dependency = root.join("dependency");
        let current = root.join("current");
        for directory in [
            vanilla.join("common/events"),
            dependency.join("common/events"),
            dependency.join("common/scripted_effects"),
            current.join("common/events"),
            current.join("common/scripted_triggers"),
            current.join("localisation/nested/deeper"),
        ] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        fs::write(
            vanilla.join("common/events/foo.txt"),
            "country_event = { id = foo.1 }\n",
        )
        .expect("vanilla event");
        fs::write(
            dependency.join("common/events/foo.txt"),
            "country_event = { id = foo.1 }\n",
        )
        .expect("dependency event");
        fs::write(
            dependency.join("common/scripted_effects/effects.txt"),
            "heal_army = { add_manpower = 1 }\n",
        )
        .expect("effect");
        let current_event = current.join("common/events/foo.txt");
        fs::write(&current_event, "country_event = { id = foo.1 }\n").expect("current event");
        fs::write(
            current.join("common/scripted_triggers/triggers.txt"),
            "is_ready = { always = yes }\n",
        )
        .expect("trigger");
        fs::write(
            current.join("localisation/nested/deeper/test_l_english.yml"),
            "l_english:\n foo_name:0 \"Foo\"\n",
        )
        .expect("localisation");
        fs::write(
            current.join("outside.yml"),
            "l_english:\n ignored_name:0 \"Ignored\"\n",
        )
        .expect("outside localisation");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot {
                id: SourceRootId::new(1),
                kind: SourceRootKind::Vanilla,
                path: vanilla,
                order: 0,
                writable: false,
            },
            SourceRoot {
                id: SourceRootId::new(2),
                kind: SourceRootKind::Dependency,
                path: dependency,
                order: 0,
                writable: false,
            },
            SourceRoot {
                id: SourceRootId::new(3),
                kind: SourceRootKind::CurrentMod,
                path: current.clone(),
                order: 0,
                writable: true,
            },
        ]));
        host.refresh_source_roots().expect("scan roots");
        let snapshot = host.snapshot();
        let event_definitions = snapshot.index().definitions("event", "foo.1");
        assert_eq!(event_definitions.len(), 3);
        assert_eq!(
            snapshot
                .index()
                .active_definition("event", "foo.1")
                .expect("active event")
                .file_id,
            event_definitions[0].file_id
        );
        assert_eq!(
            snapshot
                .index()
                .definitions("scripted_effect", "heal_army")
                .len(),
            1
        );
        assert_eq!(
            snapshot
                .index()
                .definitions("scripted_trigger", "is_ready")
                .len(),
            1
        );
        assert_eq!(
            snapshot
                .index()
                .definitions("localisation", "foo_name")
                .len(),
            1
        );
        assert!(
            snapshot
                .index()
                .definitions("localisation", "ignored_name")
                .is_empty()
        );

        let logical = LogicalPath::new("common/events/foo.txt");
        assert_eq!(
            snapshot
                .resolve(&logical)
                .iter()
                .filter(|candidate| candidate.active)
                .count(),
            1
        );
        host.open_document(
            DocumentId::new("file:///current/foo.txt"),
            1,
            "country_event = { id = foo.1 }\n".to_owned(),
            Some(current_event.clone()),
        )
        .expect("overlay");
        let overlay_snapshot = host.snapshot();
        let resolved = overlay_snapshot.resolve(&logical);
        assert!(
            resolved
                .first()
                .and_then(|candidate| candidate.document_id.as_ref())
                .is_some()
        );
        assert!(resolved.first().is_some_and(|candidate| candidate.active));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn persistent_vanilla_cache_round_trips_and_is_never_rescanned() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-engine-vanilla-cache-{nonce}"));
        let vanilla = root.join("vanilla");
        let current = root.join("current");
        fs::create_dir_all(vanilla.join("common/events")).expect("Vanilla fixture directory");
        fs::create_dir_all(vanilla.join("localisation/nested/deeper"))
            .expect("Vanilla localisation fixture directory");
        fs::create_dir_all(current.join("common/events")).expect("current fixture directory");
        fs::write(
            vanilla.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\ncountry_event = { id = vanilla.1 }\n",
        )
        .expect("Vanilla definitions");
        fs::write(
            vanilla.join("localisation/nested/deeper/test_l_english.yml"),
            "l_english:\nvanilla_name:0 \"Vanilla text\"\n",
        )
        .expect("Vanilla localisation");
        fs::write(
            current.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\n",
        )
        .expect("current definition");

        let mut vanilla_host = eu4_host();
        vanilla_host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(0),
                SourceRootKind::Vanilla,
                fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
            ),
        ]));
        vanilla_host
            .refresh_source_roots()
            .expect("scan Vanilla once");
        let vanilla_snapshot = vanilla_host.snapshot();
        assert!(vanilla_snapshot.source_files().keys().all(|file_id| {
            vanilla_snapshot
                .file_state(*file_id)
                .is_some_and(|state| state.parsed().is_none() && state.hir().is_none())
        }));
        let cache =
            VanillaIndexCache::from_snapshot(&vanilla_host.snapshot()).expect("build cache");
        let cache_path = root.join("cache/vanilla.pdxindex");
        cache.save(&cache_path).expect("save cache");
        let cancelled = WorkspaceScanToken::new();
        cancelled.cancel();
        assert!(matches!(
            VanillaIndexCache::load_cancellable(&cache_path, &cancelled),
            Err(VanillaCacheError::Cancelled)
        ));
        let loaded = VanillaIndexCache::load(&cache_path).expect("load cache");
        assert_eq!(loaded.metadata(), cache.metadata());
        assert_eq!(loaded.source_files(), cache.source_files());
        assert_eq!(loaded.index(), cache.index());
        assert_eq!(
            loaded.localisation_previews(),
            cache.localisation_previews()
        );

        let foreign_path = root.join("foreign.sqlite");
        let foreign = rusqlite::Connection::open(&foreign_path).expect("foreign database");
        foreign
            .execute("CREATE TABLE marker(value TEXT)", [])
            .expect("foreign schema");
        drop(foreign);
        assert!(matches!(
            cache.save(&foreign_path),
            Err(VanillaCacheError::NotVanillaCache)
        ));
        let foreign = rusqlite::Connection::open(&foreign_path).expect("reopen foreign database");
        assert_eq!(
            foreign
                .query_row("SELECT count(*) FROM marker", [], |row| row
                    .get::<_, i64>(0))
                .expect("foreign table remains"),
            0
        );
        drop(foreign);

        fs::rename(&vanilla, root.join("vanilla-moved")).expect("make original source unavailable");
        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(
                SourceRootId::new(u32::MAX),
                SourceRootKind::CurrentMod,
                fs::canonicalize(&current).expect("canonical current root"),
            ),
        ]));
        host.refresh_source_roots().expect("scan current root");
        host.install_vanilla_cache(loaded)
            .expect("install cache without Vanilla source access");
        host.refresh_source_roots()
            .expect("refresh must skip unavailable Vanilla root");

        let snapshot = host.snapshot();
        assert_eq!(snapshot.source_roots()[0].kind, SourceRootKind::Vanilla);
        let shared = snapshot
            .index()
            .active_definition("event", "shared.1")
            .expect("current definition wins");
        assert_eq!(
            snapshot
                .source_files()
                .get(&shared.file_id)
                .expect("shared file")
                .root_id,
            SourceRootId::new(u32::MAX)
        );
        let vanilla_definition = snapshot
            .index()
            .active_definition("event", "vanilla.1")
            .expect("cached Vanilla-only definition remains available");
        assert_eq!(
            snapshot
                .source_files()
                .get(&vanilla_definition.file_id)
                .expect("Vanilla file metadata")
                .root_id,
            SourceRootId::new(0)
        );
        assert!(snapshot.file_state(vanilla_definition.file_id).is_none());
        let vanilla_localisation = snapshot
            .index()
            .active_definition("localisation", "vanilla_name")
            .expect("cached Vanilla localisation remains available");
        let preview = snapshot
            .vanilla_localisation_preview(vanilla_localisation.file_id, vanilla_localisation.range)
            .expect("cached Vanilla localisation preview");
        assert_eq!(preview.language.as_deref(), Some("l_english"));
        assert_eq!(preview.value, "Vanilla text");
        assert!(snapshot.file_state(vanilla_localisation.file_id).is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
