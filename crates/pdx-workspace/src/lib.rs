//! Workspace state and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use pdx_hir::{HirFile, lower_shared, lower_shared_with_profile};
use pdx_rules::{FileResolutionPolicy, GameProfile, ParserKind, RuleSet, SymbolResolutionPolicy};
use pdx_syntax::{CstKind, CstNode, Eu4FileFormat, ParsedFile, parse_eu4, parse_eu4_csv_file};
use pdx_text::{LineIndex, LogicalPath, TextRange};

mod vanilla_cache;

pub use vanilla_cache::{
    CURRENT_VANILLA_CACHE_SCHEMA_VERSION, VanillaCacheError, VanillaIndexCache,
    VanillaIndexCacheMetadata,
};

#[cfg(test)]
thread_local! {
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
        Self { id, kind, path, order: id.get(), writable }
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
                if remaining != usize::MAX && remaining > 0 { Some(remaining - 1) } else { None }
            })
            .is_err_and(|remaining| remaining == 0)
        {
            self.cancel();
        }
        if self.is_cancelled() { Err(WorkspaceError::Cancelled) } else { Ok(()) }
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
    /// Source bytes were not valid UTF-8.
    InvalidUtf8,
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
    /// Classified, readable UTF-8 files added to the workspace.
    pub indexed_files: usize,
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

/// Parsed frontend retained by one immutable file state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedSource {
    /// PDX Script or localisation CST.
    Text(Arc<ParsedFile>),
    /// Structured CSV parse.
    Csv(Arc<pdx_syntax::CsvParsedFile>),
}

impl ParsedSource {
    /// Returns the common frontend format.
    #[must_use]
    pub fn format(&self) -> Eu4FileFormat {
        match self {
            Self::Text(parsed) => parsed.format(),
            Self::Csv(_) => Eu4FileFormat::Csv,
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
}

/// Workspace-wide symbol index made from immutable file shards.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceIndex {
    shards: BTreeMap<SourceFileId, FileIndexShard>,
    definitions: BTreeMap<(String, String), Vec<Definition>>,
    references: BTreeMap<SourceFileId, Vec<Reference>>,
}

impl WorkspaceIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
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

    /// Returns all retained definitions for a kind/name, including shadowed ones.
    #[must_use]
    pub fn definitions(&self, kind: &str, name: &str) -> &[Definition] {
        self.definitions
            .get(&(kind.to_owned(), name.to_ascii_lowercase()))
            .map_or(&[], Vec::as_slice)
    }

    /// Returns the active definition for a kind/name, if one exists.
    #[must_use]
    pub fn active_definition(&self, kind: &str, name: &str) -> Option<&Definition> {
        let mut active = self.definitions(kind, name).iter().filter(|definition| definition.active);
        let definition = active.next()?;
        active.next().is_none().then_some(definition)
    }

    /// Iterates over all retained definitions in deterministic kind/name order.
    #[must_use = "iterate the retained definitions"]
    pub fn definitions_iter(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.values().flat_map(|definitions| definitions.iter())
    }

    /// Returns the shard for a file.
    #[must_use]
    pub fn shard(&self, file_id: SourceFileId) -> Option<&FileIndexShard> {
        self.shards.get(&file_id)
    }

    /// Returns all references from a file.
    #[must_use]
    pub fn references(&self, file_id: SourceFileId) -> &[Reference] {
        self.references.get(&file_id).map_or(&[], Vec::as_slice)
    }

    /// Iterates over references from every retained file shard.
    #[must_use = "iterate the retained references"]
    pub fn references_iter(&self) -> impl Iterator<Item = &Reference> {
        self.references.values().flat_map(|references| references.iter())
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
        for definition in &shard.definitions {
            let key = definition_key(definition);
            self.definitions.entry(key.clone()).or_default().push(definition.clone());
            affected.push(key);
        }
        self.references.insert(file_id, shard.references.clone());
        self.shards.insert(file_id, shard);
        affected.sort();
        affected.dedup();
        self.sort_definition_buckets(&affected);
        affected
    }

    fn remove_shard_entries(&mut self, file_id: SourceFileId) -> Vec<(String, String)> {
        let Some(previous) = self.shards.remove(&file_id) else {
            self.references.remove(&file_id);
            return Vec::new();
        };
        self.references.remove(&file_id);
        let mut affected = previous.definitions.iter().map(definition_key).collect::<Vec<_>>();
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
            let Some(values) = self.definitions.get_mut(key) else { continue };
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
                .map(|definition| priorities.get(&definition.file_id).copied().unwrap_or(0))
                .max();
            for definition in values.iter_mut() {
                definition.active = match policy {
                    SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
                    SymbolResolutionPolicy::ReplaceBySymbol => {
                        Some(priorities.get(&definition.file_id).copied().unwrap_or(0)) == highest
                    }
                };
            }
            values.sort_by_key(|definition| (!definition.active, definition.file_id));
        }
    }

    fn sort_definition_buckets(&mut self, keys: &[(String, String)]) {
        for key in keys {
            if let Some(values) = self.definitions.get_mut(key) {
                values.sort_by_key(|definition| (!definition.active, definition.file_id));
            }
        }
    }

    fn rebuild_maps_cancellable(
        &mut self,
        cancellation: &WorkspaceScanToken,
    ) -> Result<(), WorkspaceError> {
        self.definitions.clear();
        self.references.clear();
        for shard in self.shards.values() {
            cancellation.checkpoint()?;
            for definition in &shard.definitions {
                cancellation.checkpoint()?;
                self.definitions
                    .entry((definition.kind.clone(), definition.name.to_ascii_lowercase()))
                    .or_default()
                    .push(definition.clone());
            }
            self.references.insert(shard.file_id, shard.references.clone());
        }
        for values in self.definitions.values_mut() {
            cancellation.checkpoint()?;
            values.sort_by_key(|definition| (!definition.active, definition.file_id));
        }
        Ok(())
    }
}

fn definition_key(definition: &Definition) -> (String, String) {
    (definition.kind.clone(), definition.name.to_ascii_lowercase())
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
        Self { range: None, text: text.into() }
    }

    /// Creates a ranged replacement.
    #[must_use]
    pub fn ranged(range: TextRange, text: impl Into<String>) -> Self {
        Self { range: Some(range), text: text.into() }
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

/// Errors raised while applying an editor document event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    /// An open notification was received for a document that is already open.
    AlreadyOpen(DocumentId),
    /// A change or close notification targeted no open overlay.
    NotOpen(DocumentId),
    /// A change version was not newer than the current version.
    StaleVersion { document: DocumentId, current: i64, received: i64 },
    /// A change range was not on UTF-8 boundaries or exceeded the current text.
    InvalidRange { document: DocumentId, range: TextRange },
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
            Self::StaleVersion { document, current, received } => write!(
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
                write!(formatter, "invalid workspace logical path: {}", path.display())
            }
            Self::FileIdCollision { first, second } => write!(
                formatter,
                "source file identity collision between {} and {}",
                first.display(),
                second.display()
            ),
            Self::FileLimitExceeded { limit } => {
                write!(formatter, "workspace contains more than the allowed {limit} files")
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
        report.issues.push(WorkspaceScanIssue { kind, path, detail });
    } else {
        report.omitted_issues = report.omitted_issues.saturating_add(1);
    }
}

fn collect_disk_files(
    root: &std::path::Path,
    current: &std::path::Path,
    depth: usize,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, PathBuf)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    cancellation.checkpoint()?;
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if depth == 0 => return Err(WorkspaceError::Io(error)),
        Err(error) => {
            record_scan_issue(
                report,
                limits,
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
                    report,
                    limits,
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
        cancellation.checkpoint()?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    path,
                    error.to_string(),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::SymlinkSkipped,
                path,
                "symbolic links are not followed during workspace discovery".to_owned(),
            );
            continue;
        }
        if file_type.is_dir() {
            if depth >= limits.max_depth {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::DepthLimitExceeded,
                    path,
                    format!(
                        "directory nesting exceeds the configured limit of {}",
                        limits.max_depth
                    ),
                );
                continue;
            }
            collect_disk_files(root, &path, depth + 1, limits, report, output, cancellation)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if report.discovered_files >= limits.max_files {
            return Err(WorkspaceError::FileLimitExceeded { limit: limits.max_files });
        }
        report.discovered_files = report.discovered_files.saturating_add(1);
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        let logical = LogicalPath::parse(&relative)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?;
        output.push((logical, path));
    }
    Ok(())
}

fn read_source_file(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
) -> Option<String> {
    read_source_file_cancellable(path, limits, report, &WorkspaceScanToken::new()).ok().flatten()
}

fn read_source_file_cancellable(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    cancellation: &WorkspaceScanToken,
) -> Result<Option<String>, WorkspaceError> {
    cancellation.checkpoint()?;
    let metadata = match fs::metadata(path) {
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
    let mut bytes = Vec::new();
    if let Err(error) = file.take(limits.max_file_size.saturating_add(1)).read_to_end(&mut bytes) {
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
            format!("file grew beyond the configured limit of {} bytes", limits.max_file_size),
        );
        return Ok(None);
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Some(text)),
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::InvalidUtf8,
                path.to_owned(),
                error.to_string(),
            );
            Ok(None)
        }
    }
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
        ParserKind::PdxScript => {
            #[cfg(test)]
            record_pipeline_parse();
            let parsed = Arc::new(parse_eu4(Eu4FileFormat::PdxScript, source));
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
            let parsed = Arc::new(parse_eu4(Eu4FileFormat::Localisation, source));
            #[cfg(test)]
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Csv(dialect) => {
            #[cfg(test)]
            record_pipeline_parse();
            let dialect = match dialect {
                pdx_rules::CsvDialect::Comma => pdx_syntax::csv::CsvDialect::Comma,
                pdx_rules::CsvDialect::Tab => pdx_syntax::csv::CsvDialect::Tab,
                pdx_rules::CsvDialect::Semicolon => pdx_syntax::csv::CsvDialect::Semicolon,
            };
            (Some(ParsedSource::Csv(Arc::new(parse_eu4_csv_file(source, dialect)))), None)
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
                path.as_str().rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase())
            })
        })?;
    let parser = match extension.as_str() {
        "yml" | "yaml" => ParserKind::Localisation,
        "csv" => ParserKind::Csv(pdx_rules::CsvDialect::Semicolon),
        "txt" | "gui" | "gfx" | "asset" | "sfx" => ParserKind::PdxScript,
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
            parse_source(&parser, &document.text, logical_path.as_ref(), rules, profile)
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
        };
    };
    let (parsed, hir) =
        parse_source(&category.parser, &source, Some(&file.logical_path), rules, profile);
    let shard = match (parsed.as_ref(), hir.as_deref()) {
        (Some(ParsedSource::Text(parsed)), Some(hir)) => {
            shard_from_parsed(file, parsed, hir, rules, profile)
        }
        (Some(ParsedSource::Text(parsed)), None) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: parsed.errors().len(),
        },
        (Some(ParsedSource::Csv(parsed)), _) => {
            let definitions = collect_profile_csv_definitions(file, parsed, profile);
            FileIndexShard {
                file_id: file.id,
                definitions,
                references: Vec::new(),
                syntax_error_count: parsed.errors().len(),
            }
        }
        (None, _) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: 0,
        },
    };
    FileState { revision, source: Arc::from(source), parsed, hir, shard: Arc::new(shard) }
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
    collect_profile_token_definitions(file, parsed, profile, &mut definitions);
    collect_cwt_type_members(file, parsed, rules, &mut definitions);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        syntax_error_count: parsed.errors().len(),
    }
}

fn collect_profile_csv_definitions(
    file: &SourceFile,
    parsed: &pdx_syntax::CsvParsedFile,
    profile: &GameProfile,
) -> Vec<Definition> {
    profile
        .csv_definitions
        .iter()
        .filter(|rule| rule.path.matches(file.logical_path.as_str()))
        .flat_map(|rule| {
            parsed.parse().records.iter().filter_map(move |record| {
                let cell = record.cells.get(rule.column)?;
                let name = parsed.source()[usize::try_from(cell.value_range.start()).ok()?
                    ..usize::try_from(cell.value_range.end()).ok()?]
                    .trim()
                    .to_owned();
                if rule.unsigned_integer_only && name.parse::<u32>().is_err() {
                    return None;
                }
                Some(Definition {
                    kind: rule.kind.clone(),
                    name,
                    file_id: file.id,
                    range: cell.value_range,
                    active: true,
                })
            })
        })
        .collect()
}

/// Collects workspace members declared by CWT `type[...]` definitions.
///
/// CWTools builds these members from the parsed workspace, rather than treating a type's name as
/// a literal root key. For example, `type[mission]` with `skip_root_key = any` exposes every child
/// of every root clause in `missions/*.txt` as a `<mission>` member. Keeping this in the workspace
/// shard makes CWT key/value matching, completion, and hover see the same dynamic names.
fn collect_cwt_type_members(
    file: &SourceFile,
    parsed: &ParsedFile,
    rules: &RuleSet,
    definitions: &mut Vec<Definition>,
) {
    for descriptor in rules.model().cwt.type_descriptors.values() {
        if !cwt_type_path_matches(descriptor, &file.logical_path) {
            continue;
        }

        if descriptor.type_per_file {
            let Some(file_name) = file.logical_path.as_str().rsplit('/').next() else {
                continue;
            };
            let name = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
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
                    collect_cwt_type_definition(file, parsed, descriptor, child, definitions);
                }
            }
        } else {
            for root in parsed.root().children() {
                if root.kind() != CstKind::Property {
                    continue;
                }
                for skip_path in &descriptor.skip_root_paths {
                    collect_cwt_skip_root_path(
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

fn collect_cwt_skip_root_path(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::CwtTypeDescriptor,
    node: &CstNode,
    path: &[String],
    definitions: &mut Vec<Definition>,
) {
    let Some(head) = path.first() else {
        collect_cwt_block_children(file, parsed, descriptor, node, definitions);
        return;
    };
    let node_key = cwt_property_key(node, parsed).unwrap_or_default();
    if !head.eq_ignore_ascii_case("any") && !head.eq_ignore_ascii_case(&node_key) {
        return;
    }
    if path.len() == 1 {
        collect_cwt_block_children(file, parsed, descriptor, node, definitions);
        return;
    }
    for child in cwt_block_properties(node) {
        collect_cwt_skip_root_path(file, parsed, descriptor, child, &path[1..], definitions);
    }
}

fn collect_cwt_block_children(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::CwtTypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    for child in cwt_block_properties(node) {
        collect_cwt_type_definition(file, parsed, descriptor, child, definitions);
    }
}

fn collect_cwt_type_definition(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::CwtTypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    let Some(key) = cwt_property_key(node, parsed) else { return };
    if !cwt_type_key_matches(descriptor, &key) {
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

fn cwt_type_key_matches(descriptor: &pdx_rules::CwtTypeDescriptor, key: &str) -> bool {
    descriptor.type_key_filter.as_ref().is_none_or(|(values, negate)| {
        (values.iter().any(|value| value.eq_ignore_ascii_case(key))) != *negate
    })
}

fn cwt_block_properties(node: &CstNode) -> impl Iterator<Item = &CstNode> {
    node.children().iter().flat_map(|child| {
        if child.kind() != CstKind::Value {
            return Vec::new();
        }
        child
            .children()
            .iter()
            .filter(|block| block.kind() == CstKind::Block)
            .flat_map(|block| {
                block.children().iter().filter(|child| child.kind() == CstKind::Property)
            })
            .collect::<Vec<_>>()
    })
}

fn cwt_property_key(node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    node.children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)
        .and_then(|child| parsed.text(child.range()))
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn cwt_type_path_matches(
    descriptor: &pdx_rules::CwtTypeDescriptor,
    logical_path: &LogicalPath,
) -> bool {
    let path = logical_path.as_str().replace('\\', "/").to_ascii_lowercase();
    let (directory, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix =
            prefix.trim_matches('/').strip_prefix("game/").unwrap_or(prefix.trim_matches('/'));
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
        let actual_extension = file_name.rsplit_once('.').map_or("", |(_, extension)| extension);
        if !actual_extension.eq_ignore_ascii_case(expected_extension) {
            return false;
        }
    }
    true
}

fn collect_profile_token_definitions(
    file: &SourceFile,
    parsed: &ParsedFile,
    profile: &GameProfile,
    definitions: &mut Vec<Definition>,
) {
    for rule in profile
        .token_definitions
        .iter()
        .filter(|rule| rule.path.matches(file.logical_path.as_str()))
    {
        for token in
            parsed.tokens().iter().filter(|token| token.kind() == pdx_syntax::TokenKind::Bare)
        {
            let Some(raw) = parsed.text(token.range()) else { continue };
            let mut opening: Option<usize> = None;
            for (offset, character) in raw.char_indices() {
                if character != rule.delimiter {
                    continue;
                }
                if let Some(start) = opening.take() {
                    if start + rule.delimiter.len_utf8() < offset {
                        let name_start = start.saturating_add(rule.delimiter.len_utf8());
                        let name = raw[name_start..offset].to_owned();
                        let token_start = usize::try_from(token.range().start()).unwrap_or(0);
                        let start =
                            u32::try_from(token_start.saturating_add(start)).unwrap_or(u32::MAX);
                        let end = u32::try_from(
                            token_start.saturating_add(offset.saturating_add(character.len_utf8())),
                        )
                        .unwrap_or(u32::MAX);
                        let range = TextRange::new(start, end).unwrap_or(token.range());
                        definitions.push(Definition {
                            kind: rule.inner_kind.clone(),
                            name: name.clone(),
                            file_id: file.id,
                            range,
                            active: true,
                        });
                        definitions.push(Definition {
                            kind: rule.wrapped_kind.clone(),
                            name: format!("{}{name}{}", rule.delimiter, rule.delimiter),
                            file_id: file.id,
                            range,
                            active: true,
                        });
                    }
                } else {
                    opening = Some(offset);
                }
            }
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
                if child.kind() == CstKind::Value {
                    if let Some(value) = child.children().iter().find(|value| {
                        matches!(value.kind(), CstKind::BareValue | CstKind::QuotedString)
                    }) {
                        return parsed
                            .text(value.range())
                            .map(|value| value.trim_matches('"').trim().to_owned());
                    }
                }
            }
        }
    }
    node.children().iter().find_map(|child| find_property(child, wanted, parsed))
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
    vanilla_cache: Option<Arc<VanillaIndexCache>>,
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
            vanilla_cache: None,
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
                self.vanilla_cache = None;
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

        let mut files = cache.source_files().clone();
        for (id, file) in self.source_files.iter() {
            if let Some(cached) = files.insert(*id, file.clone()) {
                return Err(VanillaCacheError::InvalidData(format!(
                    "file id collision between {} and {}",
                    cached.physical_path.display(),
                    file.physical_path.display()
                )));
            }
        }
        let mut shards = cache.index().shards.values().cloned().collect::<Vec<_>>();
        shards.extend(self.index.shards.values().cloned());
        let mut roots = Vec::with_capacity(self.roots.len().saturating_add(1));
        roots.push(vanilla.clone());
        roots.extend(self.roots.iter().cloned());
        let mut index = WorkspaceIndex::from_shards(shards);
        let priorities = source_priorities(&roots, &files);
        index.resolve_priorities(&priorities, self.rules.as_ref());

        self.roots = Arc::from(roots);
        self.source_files = Arc::new(files);
        self.index = Arc::new(index);
        self.vanilla_cache = Some(Arc::new(cache));
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
        )
    }

    /// Scans all configured roots with explicit resource limits.
    pub fn refresh_source_roots_with_limits(
        &mut self,
        limits: WorkspaceScanLimits,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(limits, &WorkspaceScanToken::new())
    }

    /// Scans configured roots with explicit resource limits and cooperative cancellation.
    pub fn refresh_source_roots_with_limits_and_cancellation(
        &mut self,
        limits: WorkspaceScanLimits,
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        cancellation.checkpoint()?;
        let mut files: BTreeMap<SourceFileId, SourceFile> = BTreeMap::new();
        let mut texts = BTreeMap::new();
        let mut report = WorkspaceScanReport::default();
        for root in self.roots.iter() {
            cancellation.checkpoint()?;
            if self.vanilla_cache.as_ref().is_some_and(|cache| cache.source_root().id == root.id) {
                continue;
            }
            let mut paths = Vec::new();
            collect_disk_files(
                &root.path,
                &root.path,
                0,
                limits,
                &mut report,
                &mut paths,
                cancellation,
            )?;
            paths.sort_by(|left, right| left.0.cmp(&right.0));
            for (logical, physical) in paths {
                cancellation.checkpoint()?;
                let id = SourceFileId::new(stable_file_id(root.id, &logical));
                let Some(category) = self.rules.classify(&logical) else { continue };
                let Some(text) =
                    read_source_file_cancellable(&physical, limits, &mut report, cancellation)?
                else {
                    continue;
                };
                if let Some(existing) = files.get(&id) {
                    return Err(WorkspaceError::FileIdCollision {
                        first: existing.physical_path.clone(),
                        second: physical,
                    });
                }
                let source_file = SourceFile {
                    id,
                    root_id: root.id,
                    physical_path: physical,
                    logical_path: logical,
                    category_id: Some(category.id.clone()),
                    resolution: category.resolution,
                };
                files.insert(id, source_file);
                texts.insert(id, text);
                report.indexed_files = report.indexed_files.saturating_add(1);
            }
        }
        let mut file_states = BTreeMap::new();
        for (id, file) in &files {
            cancellation.checkpoint()?;
            let Some(text) = texts.remove(id) else { continue };
            let state = match self.file_states.get(id) {
                Some(previous)
                    if self.source_files.get(id) == Some(file) && previous.source() == text =>
                {
                    Arc::clone(previous)
                }
                previous => {
                    let file_revision =
                        previous.map_or(0, |state| state.revision().saturating_add(1));
                    Arc::new(build_file_state(
                        file,
                        text,
                        file_revision,
                        self.rules.as_ref(),
                        self.profile.as_ref(),
                    ))
                }
            };
            file_states.insert(*id, state);
        }
        cancellation.checkpoint()?;
        let mut shards =
            file_states.values().map(|state| state.shard().clone()).collect::<Vec<_>>();
        if let Some(cache) = self.vanilla_cache.as_ref() {
            for (id, cached) in cache.source_files() {
                if let Some(existing) = files.insert(*id, cached.clone()) {
                    return Err(WorkspaceError::FileIdCollision {
                        first: existing.physical_path,
                        second: cached.physical_path.clone(),
                    });
                }
            }
            shards.extend(cache.index().shards.values().cloned());
        }
        let mut index = WorkspaceIndex::from_shards_cancellable(shards, cancellation)?;
        let priorities = source_priorities(&self.roots, &files);
        index.resolve_priorities_cancellable(&priorities, self.rules.as_ref(), cancellation)?;
        cancellation.checkpoint()?;
        self.source_files = Arc::new(files);
        self.file_states = Arc::new(file_states);
        self.index = Arc::new(index);
        self.scan_report = Arc::new(report.clone());
        self.revision = self.revision.saturating_add(1);
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
        if let Some(previous) = self.file_states.get(&file_id) {
            let mut replacement = previous.as_ref().clone();
            replacement.shard = Arc::new(shard);
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
        let document =
            self.document_snapshot(id.clone(), Some(version), text, DocumentSource::Overlay, path);
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
        let Some(current) = self.documents.get(&id) else { return false };
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
                    return Err(DocumentError::InvalidRange { document: id.clone(), range });
                }
                if let (Some(start), Some(end)) = (start, end) {
                    text.replace_range(start..end, &change.text);
                }
            } else {
                text = change.text.clone();
            }
        }

        let path = current.path.clone();
        let document =
            self.document_snapshot(id.clone(), Some(version), text, DocumentSource::Overlay, path);
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
            if let Some(text) = read_source_file(&path, WorkspaceScanLimits::default(), &mut report)
            {
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
                let priority =
                    self.roots.iter().find(|root| root.id == file.root_id).map_or(0, root_priority);
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
            let Some(path) = document.path() else { continue };
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
        let overlay_present = candidates.iter().any(|candidate| candidate.document_id.is_some());
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;

    use super::{
        AnalysisHost, Definition, DocumentId, DocumentSource, FileIndexShard, ParsedSource,
        Reference, SourceFileId, SourceRoot, SourceRootId, SourceRootKind, TextChange,
        VanillaCacheError, VanillaIndexCache, WorkspaceError, WorkspaceIndex,
        WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanToken, pipeline_counts,
        reset_pipeline_counts,
    };
    use pdx_rules::RuleSet;
    use pdx_text::{LogicalPath, TextRange};

    fn eu4_host() -> AnalysisHost {
        AnalysisHost::with_profile(pdx_game_eu4::bootstrap_rules(), pdx_game_eu4::profile())
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
    fn identity_only_host_does_not_leak_eu4_dynamic_symbols() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-generic-profile-{nonce}"));
        let cultures = root.join("common/cultures");
        let scripted_effects = root.join("common/scripted_effects");
        let map = root.join("map");
        for directory in [&cultures, &scripted_effects, &map] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        fs::write(
            cultures.join("cultures.txt"),
            "germanic = { set_country_flag = generic_flag }\n",
        )
        .expect("culture fixture");
        fs::write(scripted_effects.join("effects.txt"), "example = { value = $AMOUNT$ }\n")
            .expect("scripted effect fixture");
        fs::write(map.join("definition.csv"), "1;1;2;3;generic;x\n").expect("province fixture");

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        host.refresh_source_roots().expect("scan roots");
        let snapshot = host.snapshot();

        assert!(snapshot.index().definitions("culture", "germanic").is_empty());
        assert!(snapshot.index().definitions("country_flag", "generic_flag").is_empty());
        assert!(snapshot.index().definitions("scripted_effect_param", "AMOUNT").is_empty());
        assert!(snapshot.index().definitions("province_id", "1").is_empty());
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
        assert_eq!(index.shard(first_file).expect("replacement shard").syntax_error_count, 1);

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
        let rules = pdx_game_eu4::bootstrap_rules();
        let tied = BTreeMap::from([(first_file, 10), (second_file, 10)]);
        index.resolve_priorities(&tied, &rules);
        assert_eq!(
            index.definitions("event", "shared.1").iter().filter(|item| item.active).count(),
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
        index.replace_shard_resolved(
            FileIndexShard {
                file_id: second_file,
                definitions: Vec::new(),
                references: Vec::new(),
                syntax_error_count: 0,
            },
            &ordered,
            &rules,
        );
        assert_eq!(
            index.active_definition("event", "shared.1").expect("remaining definition").file_id,
            first_file
        );
    }

    #[test]
    fn source_file_ids_do_not_shift_when_an_earlier_path_is_added() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-stable-ids-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("b.txt"), "country_event = { id = stable.b }\n").expect("b event");
        fs::write(events.join("c.txt"), "country_event = { id = stable.c }\n").expect("c event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
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
        let root = std::env::temp_dir().join(format!("pdx-workspace-file-state-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("a.txt"), "country_event = { id = state.a }\n").expect("a event");
        fs::write(events.join("b.txt"), "country_event = { id = state.b }\n").expect("b event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
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
            first.file_state(a).expect("a state").hir().expect("a HIR").syntax()
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

        fs::write(events.join("b.txt"), "country_event = { id = state.changed }\n")
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
            second.file_state(b).expect("old b state").revision().saturating_add(1)
        );
        assert_eq!(third.index().definitions("event", "state.changed").len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn one_overlay_edit_parses_and_lowers_exactly_once_in_a_populated_workspace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-pipeline-count-{nonce}"));
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
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
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
        let initial = host.snapshot().prepare_document(&id).expect("prepare initial overlay");
        assert!(host.commit_prepared_document(initial));
        let before_edit = host.snapshot();

        reset_pipeline_counts();
        host.stage_document_text(&id, 2, "country_event = { id = synthetic.changed }\n".to_owned())
            .expect("stage edit");
        assert_eq!(pipeline_counts(), (0, 0), "staging must not run semantic work");

        let prepared = host.snapshot().prepare_document(&id).expect("prepare edited overlay");
        assert_eq!(pipeline_counts(), (1, 1));
        assert!(host.commit_prepared_document(prepared));
        assert_eq!(pipeline_counts(), (1, 1), "commit must not repeat worker work");

        let after_edit = host.snapshot();
        for file_id in before_edit.source_files().keys() {
            assert!(Arc::ptr_eq(
                before_edit.file_states.get(file_id).expect("old disk state"),
                after_edit.file_states.get(file_id).expect("current disk state"),
            ));
        }
        assert!(after_edit.document(&id).expect("edited overlay").hir().is_some_and(|hir| {
            hir.definitions().iter().any(|definition| definition.name == "synthetic.changed")
        }));
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
        host.open_document(id.clone(), 1, "one".to_owned(), None).expect("open should succeed");
        let third = host.snapshot();

        assert!(first.document(&id).is_none());
        assert_eq!(third.document(&id).expect("new snapshot sees document").text(), "one");
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
        let root = std::env::temp_dir().join(format!("pdx-workspace-isolation-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("good.txt"), "country_event = { id = safe.1 }\n")
            .expect("valid event");
        fs::write(events.join("invalid.txt"), [0xff, 0xfe]).expect("invalid UTF-8 event");
        fs::write(events.join("large.txt"), vec![b'x'; 65]).expect("oversized event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        let report = host
            .refresh_source_roots_with_limits(WorkspaceScanLimits {
                max_file_size: 64,
                ..WorkspaceScanLimits::default()
            })
            .expect("recoverable file failures should not abort scanning");

        assert_eq!(report.discovered_files, 3);
        assert_eq!(report.indexed_files, 1);
        assert_eq!(report.skipped_entries, 2);
        assert!(
            report.issues.iter().any(|issue| issue.kind == WorkspaceScanIssueKind::InvalidUtf8)
        );
        assert!(
            report.issues.iter().any(|issue| issue.kind == WorkspaceScanIssueKind::FileTooLarge)
        );
        assert_eq!(host.snapshot().scan_report(), &report);
        assert_eq!(host.snapshot().source_files().len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn depth_limit_skips_nested_subtrees_with_a_reported_issue() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-depth-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("deep.txt"), "country_event = { id = deep.1 }\n")
            .expect("deep event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
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
        let root = std::env::temp_dir().join(format!("pdx-workspace-file-limit-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("a.txt"), "country_event = { id = limit.a }\n").expect("a event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        host.refresh_source_roots().expect("initial scan");
        let before = host.snapshot();
        fs::write(events.join("b.txt"), "country_event = { id = limit.b }\n").expect("b event");

        let error = host
            .refresh_source_roots_with_limits(WorkspaceScanLimits {
                max_files: 1,
                ..WorkspaceScanLimits::default()
            })
            .expect_err("the total file limit must be enforced");
        assert!(matches!(error, super::WorkspaceError::FileLimitExceeded { limit: 1 }));
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
        let root = std::env::temp_dir().join(format!("pdx-workspace-cancel-scan-{nonce}"));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("event directory");
        fs::write(events.join("baseline.txt"), "country_event = { id = baseline.1 }\n")
            .expect("baseline event");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
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

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_reported_and_never_followed() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-symlink-root-{nonce}"));
        let outside = std::env::temp_dir().join(format!("pdx-workspace-symlink-outside-{nonce}"));
        fs::create_dir_all(&root).expect("source root");
        fs::create_dir_all(&outside).expect("outside directory");
        fs::write(outside.join("leak.txt"), "country_event = { id = leak.1 }\n")
            .expect("outside event");
        symlink(&outside, root.join("events")).expect("directory symlink");

        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            root.clone(),
        )]));
        let report = host.refresh_source_roots().expect("symlink-safe scan");

        assert_eq!(report.discovered_files, 0);
        assert_eq!(report.indexed_files, 0);
        assert!(
            report.issues.iter().any(|issue| issue.kind == WorkspaceScanIssueKind::SymlinkSkipped)
        );
        fs::remove_dir_all(root).expect("cleanup root");
        fs::remove_dir_all(outside).expect("cleanup outside directory");
    }

    #[test]
    fn stale_document_versions_are_rejected_atomically() {
        let mut host = AnalysisHost::new(RuleSet::empty());
        let id = DocumentId::new("file:///tmp/example.txt");
        host.open_document(id.clone(), 1, "a😀z".to_owned(), None).expect("open should succeed");
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
        assert_eq!(host.snapshot().document(&id).expect("document exists").text(), "a😀z");
        host.apply_document_changes(&id, 2, &[TextChange::ranged(range, "x")])
            .expect("new version should succeed");
        let second = host.snapshot();
        let second_document = second.document(&id).expect("document exists");
        assert_eq!(second_document.text(), "axz");
        let Some(ParsedSource::Text(second_parse)) = second_document.parsed() else {
            panic!("changed txt overlay should retain a text parse");
        };
        assert!(!Arc::ptr_eq(first_parse, second_parse));
        assert_eq!(first.document(&id).expect("old snapshot remains valid").text(), "a😀z");
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
        assert!(staged.document(&id).expect("staged document").parsed().is_none());
        let stale = staged.prepare_document(&id).expect("prepare stale candidate");

        host.stage_document_text(&id, 2, "country_event = { id = current.1 }\n".to_owned())
            .expect("stage newer text");
        assert!(!host.commit_prepared_document(stale));
        let current = host.snapshot().prepare_document(&id).expect("prepare current candidate");
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
        let path = std::env::temp_dir().join(format!("pdx-workspace-{}.txt", std::process::id()));
        fs::write(&path, "disk").expect("write fixture");
        let mut host = AnalysisHost::new(RuleSet::empty());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            path.parent().expect("temp parent").to_owned(),
        )]));
        let id = DocumentId::new("file:///tmp/pdx-workspace.txt");
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
        let root = std::env::temp_dir().join(format!("pdx-workspace-phase4-{nonce}"));
        let vanilla = root.join("vanilla");
        let dependency = root.join("dependency");
        let current = root.join("current");
        for directory in [
            vanilla.join("common/events"),
            dependency.join("common/events"),
            dependency.join("common/scripted_effects"),
            current.join("common/events"),
            current.join("common/scripted_triggers"),
            current.join("localisation"),
        ] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        fs::write(vanilla.join("common/events/foo.txt"), "country_event = { id = foo.1 }\n")
            .expect("vanilla event");
        fs::write(dependency.join("common/events/foo.txt"), "country_event = { id = foo.1 }\n")
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
            current.join("localisation/test_l_english.yml"),
            "l_english:\n foo_name:0 \"Foo\"\n",
        )
        .expect("localisation");

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
            snapshot.index().active_definition("event", "foo.1").expect("active event").file_id,
            event_definitions[0].file_id
        );
        assert_eq!(snapshot.index().definitions("scripted_effect", "heal_army").len(), 1);
        assert_eq!(snapshot.index().definitions("scripted_trigger", "is_ready").len(), 1);
        assert_eq!(snapshot.index().definitions("localisation", "foo_name").len(), 1);

        let logical = LogicalPath::new("common/events/foo.txt");
        assert_eq!(
            snapshot.resolve(&logical).iter().filter(|candidate| candidate.active).count(),
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
        assert!(resolved.first().and_then(|candidate| candidate.document_id.as_ref()).is_some());
        assert!(resolved.first().is_some_and(|candidate| candidate.active));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn persistent_vanilla_cache_round_trips_and_is_never_rescanned() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-vanilla-cache-{nonce}"));
        let vanilla = root.join("vanilla");
        let current = root.join("current");
        fs::create_dir_all(vanilla.join("common/events")).expect("Vanilla fixture directory");
        fs::create_dir_all(current.join("common/events")).expect("current fixture directory");
        fs::write(
            vanilla.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\ncountry_event = { id = vanilla.1 }\n",
        )
        .expect("Vanilla definitions");
        fs::write(
            current.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\n",
        )
        .expect("current definition");

        let mut vanilla_host = eu4_host();
        vanilla_host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        )]));
        vanilla_host.refresh_source_roots().expect("scan Vanilla once");
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

        let foreign_path = root.join("foreign.sqlite");
        let foreign = rusqlite::Connection::open(&foreign_path).expect("foreign database");
        foreign.execute("CREATE TABLE marker(value TEXT)", []).expect("foreign schema");
        drop(foreign);
        assert!(matches!(cache.save(&foreign_path), Err(VanillaCacheError::NotVanillaCache)));
        let foreign = rusqlite::Connection::open(&foreign_path).expect("reopen foreign database");
        assert_eq!(
            foreign
                .query_row("SELECT count(*) FROM marker", [], |row| row.get::<_, i64>(0))
                .expect("foreign table remains"),
            0
        );
        drop(foreign);

        fs::rename(&vanilla, root.join("vanilla-moved")).expect("make original source unavailable");
        let mut host = eu4_host();
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(u32::MAX),
            SourceRootKind::CurrentMod,
            fs::canonicalize(&current).expect("canonical current root"),
        )]));
        host.refresh_source_roots().expect("scan current root");
        host.install_vanilla_cache(loaded).expect("install cache without Vanilla source access");
        host.refresh_source_roots().expect("refresh must skip unavailable Vanilla root");

        let snapshot = host.snapshot();
        assert_eq!(snapshot.source_roots()[0].kind, SourceRootKind::Vanilla);
        let shared = snapshot
            .index()
            .active_definition("event", "shared.1")
            .expect("current definition wins");
        assert_eq!(
            snapshot.source_files().get(&shared.file_id).expect("shared file").root_id,
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
        fs::remove_dir_all(root).expect("cleanup");
    }
}
