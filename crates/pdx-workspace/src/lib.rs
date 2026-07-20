//! Workspace state and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdx_hir::{HirFile, HirProperty, lower_shared};
use pdx_rules::{FileResolutionPolicy, ParserKind, RuleSet, SymbolResolutionPolicy};
use pdx_syntax::{CstKind, CstNode, Eu4FileFormat, ParsedFile, parse_eu4, parse_eu4_csv_file};
use pdx_text::{LineIndex, LogicalPath, TextRange};

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
    /// EU4 logical path relative to the root.
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
        let mut index = Self::empty();
        index.shards.extend(shards.into_iter().map(|shard| (shard.file_id, shard)));
        index.rebuild_maps();
        index
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
        let keys = self.definitions.keys().cloned().collect::<Vec<_>>();
        self.resolve_definition_buckets(&keys, priorities, rules);
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

    fn rebuild_maps(&mut self) {
        self.definitions.clear();
        self.references.clear();
        for shard in self.shards.values() {
            for definition in &shard.definitions {
                self.definitions
                    .entry((definition.kind.clone(), definition.name.to_ascii_lowercase()))
                    .or_default()
                    .push(definition.clone());
            }
            self.references.insert(shard.file_id, shard.references.clone());
        }
        for values in self.definitions.values_mut() {
            values.sort_by_key(|definition| (!definition.active, definition.file_id));
        }
    }
}

fn definition_key(definition: &Definition) -> (String, String) {
    (definition.kind.clone(), definition.name.to_ascii_lowercase())
}

/// Vanilla cache metadata. Loading is explicit; a host never refreshes it implicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanillaIndexCacheMetadata {
    /// Cache format version.
    pub schema_version: u32,
    /// Rules hash used to create the cache.
    pub rule_hash: String,
    /// Source identity supplied by the caller.
    pub source_identity: String,
}

/// Explicitly managed Vanilla index cache seam.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VanillaIndexCache {
    metadata: Option<VanillaIndexCacheMetadata>,
    index: WorkspaceIndex,
}

impl VanillaIndexCache {
    /// Creates an unconfigured cache. It never scans or refreshes by itself.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns cache metadata when a caller has explicitly installed a snapshot.
    #[must_use]
    pub fn metadata(&self) -> Option<&VanillaIndexCacheMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the cached immutable index.
    #[must_use]
    pub fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Installs a newly rebuilt cache. This is the only refresh operation.
    pub fn refresh(&mut self, metadata: VanillaIndexCacheMetadata, index: WorkspaceIndex) {
        self.metadata = Some(metadata);
        self.index = index;
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
) -> Result<(), WorkspaceError> {
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
            collect_disk_files(root, &path, depth + 1, limits, report, output)?;
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
            return None;
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
        return None;
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
            return None;
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
        return None;
    }
    if u64::try_from(bytes.len()).map_or(true, |size| size > limits.max_file_size) {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileTooLarge,
            path.to_owned(),
            format!("file grew beyond the configured limit of {} bytes", limits.max_file_size),
        );
        return None;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::InvalidUtf8,
                path.to_owned(),
                error.to_string(),
            );
            None
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
    rules: &RuleSet,
) -> (Option<ParsedSource>, Option<Arc<HirFile>>) {
    match parser {
        ParserKind::PdxScript => {
            let parsed = Arc::new(parse_eu4(Eu4FileFormat::PdxScript, source));
            let hir = Arc::new(lower_shared(Arc::clone(&parsed), rules));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Localisation => {
            let parsed = Arc::new(parse_eu4(Eu4FileFormat::Localisation, source));
            let hir = Arc::new(lower_shared(Arc::clone(&parsed), rules));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Csv(dialect) => {
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

fn build_file_state(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
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
    let (parsed, hir) = parse_source(&category.parser, &source, rules);
    let shard = match (parsed.as_ref(), hir.as_deref()) {
        (Some(ParsedSource::Text(parsed)), Some(hir)) => {
            shard_from_parsed(file, parsed, hir, category.id.as_str(), rules)
        }
        (Some(ParsedSource::Text(parsed)), None) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: parsed.errors().len(),
        },
        (Some(ParsedSource::Csv(parsed)), _) => {
            let definitions =
                if file.logical_path.as_str().eq_ignore_ascii_case("map/definition.csv") {
                    parsed
                        .parse()
                        .records
                        .iter()
                        .flat_map(|record| record.cells.first())
                        .filter_map(|cell| {
                            let name = parsed.source()[usize::try_from(cell.value_range.start())
                                .ok()?
                                ..usize::try_from(cell.value_range.end()).ok()?]
                                .trim()
                                .to_owned();
                            name.parse::<u32>().ok().map(|_| Definition {
                                kind: "province_id".to_owned(),
                                name,
                                file_id: file.id,
                                range: cell.value_range,
                                active: true,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
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
    category_id: &str,
    rules: &RuleSet,
) -> FileIndexShard {
    let mut definitions = Vec::new();
    let mut references = Vec::new();
    collect_hir_semantics(file, hir, category_id, &mut definitions, &mut references);
    collect_scripted_effect_params(file, parsed, &mut definitions);
    collect_eu4_dynamic_members(file, hir, &mut definitions);
    collect_cwt_type_members(file, parsed, rules, &mut definitions);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        syntax_error_count: parsed.errors().len(),
    }
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

fn collect_eu4_dynamic_members(
    file: &SourceFile,
    hir: &HirFile,
    definitions: &mut Vec<Definition>,
) {
    for property in hir.properties() {
        let key = property.key.to_ascii_lowercase();
        let parent_key = property.path.iter().rev().nth(1).map(String::as_str);
        let kind = dynamic_member_kind(&key, parent_key);
        if let Some(kind) = kind
            && let Some(scalar) = property.scalar.as_ref()
            && !scalar.value.is_empty()
        {
            definitions.push(Definition {
                kind: kind.to_owned(),
                name: scalar.value.clone(),
                file_id: file.id,
                range: scalar.range,
                active: true,
            });
        }
    }
}

fn dynamic_member_kind(key: &str, parent_key: Option<&str>) -> Option<&'static str> {
    Some(match key {
        "set_country_flag" => "country_flag",
        "set_global_flag" => "global_flag",
        "set_province_flag" => "province_flag",
        "set_ruler_flag" => "ruler_flag",
        "set_heir_flag" => "heir_flag",
        "set_consort_flag" => "consort_flag",
        "save_event_target_as" => "event_target",
        "save_global_event_target_as" => "global_event_target",
        "set_saved_name" => "saved_name",
        "which"
            if matches!(
                parent_key,
                Some("set_variable")
                    | Some("change_variable")
                    | Some("new_variable")
                    | Some("new_variables")
            ) =>
        {
            "variable"
        }
        _ => return None,
    })
}

fn collect_scripted_effect_params(
    file: &SourceFile,
    parsed: &ParsedFile,
    definitions: &mut Vec<Definition>,
) {
    let path = file.logical_path.as_str().to_ascii_lowercase();
    if !(path.starts_with("common/scripted_effects/")
        || path.starts_with("common/scripted_triggers/"))
    {
        return;
    }
    for token in parsed.tokens().iter().filter(|token| token.kind() == pdx_syntax::TokenKind::Bare)
    {
        let Some(raw) = parsed.text(token.range()) else { continue };
        let mut opening = None;
        for (offset, character) in raw.char_indices() {
            if character != '$' {
                continue;
            }
            if let Some(start) = opening.take() {
                if start + 1 < offset {
                    let name = raw[start + 1..offset].to_owned();
                    let token_start = usize::try_from(token.range().start()).unwrap_or(0);
                    let start =
                        u32::try_from(token_start.saturating_add(start)).unwrap_or(u32::MAX);
                    let end = u32::try_from(
                        token_start.saturating_add(offset.saturating_add(character.len_utf8())),
                    )
                    .unwrap_or(u32::MAX);
                    let range = TextRange::new(start, end).unwrap_or(token.range());
                    definitions.push(Definition {
                        kind: "scripted_effect_param".to_owned(),
                        name: name.clone(),
                        file_id: file.id,
                        range,
                        active: true,
                    });
                    definitions.push(Definition {
                        kind: "scripted_effect_param_dollar".to_owned(),
                        name: format!("${name}$"),
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

fn collect_hir_semantics(
    file: &SourceFile,
    hir: &HirFile,
    category_id: &str,
    definitions: &mut Vec<Definition>,
    references: &mut Vec<Reference>,
) {
    for entry in hir.localisation_entries() {
        definitions.push(Definition {
            kind: "localisation".to_owned(),
            name: entry.name.clone(),
            file_id: file.id,
            range: entry.range,
            active: true,
        });
    }
    let logical_path = file.logical_path.as_str().to_ascii_lowercase();
    for property in hir.properties() {
        if property.top_level {
            if let Some(kind) = definition_kind(&logical_path, property) {
                let name = event_name(hir, property).unwrap_or_else(|| property.key.clone());
                definitions.push(Definition {
                    kind,
                    name,
                    file_id: file.id,
                    range: property.range,
                    active: true,
                });
                if logical_path.contains("common/government_reforms/")
                    && nested_hir_property(hir, property, "legacy_government")
                        .and_then(|property| property.scalar.as_ref())
                        .is_some_and(|scalar| scalar.value.eq_ignore_ascii_case("yes"))
                    && nested_hir_property(hir, property, "legacy_equivalent").is_none()
                {
                    definitions.push(Definition {
                        kind: "hardcoded_legacy_government".to_owned(),
                        name: property.key.clone(),
                        file_id: file.id,
                        range: property.range,
                        active: true,
                    });
                }
            }
            if logical_path.contains("common/country_tags")
                && property.key.eq_ignore_ascii_case("countries")
            {
                for country in hir.properties().iter().filter(|candidate| {
                    candidate.path.len() == property.path.len().saturating_add(1)
                        && candidate.path.starts_with(&property.path)
                        && range_within(candidate.range, property.range)
                }) {
                    definitions.push(Definition {
                        kind: "country_tag".to_owned(),
                        name: country.key.clone(),
                        file_id: file.id,
                        range: country.range,
                        active: true,
                    });
                }
            }
        }
        if let Some((kind, name, range)) = semantic_reference(property) {
            references.push(Reference { kind, name, file_id: file.id, range });
        }
    }
    for value in hir.bare_values() {
        references.push(Reference {
            kind: category_id.to_owned(),
            name: value.value.clone(),
            file_id: file.id,
            range: value.range,
        });
    }
}

fn semantic_reference(property: &HirProperty) -> Option<(String, String, TextRange)> {
    let lower = property.key.to_ascii_lowercase();
    let kind = if matches!(lower.as_str(), "event" | "events" | "event_id" | "trigger_event")
        || lower.ends_with("_event")
    {
        Some("event")
    } else if lower.contains("scripted_effect")
        || lower == "call_effect"
        || lower.ends_with("_effect")
    {
        Some("scripted_effect")
    } else if lower.contains("scripted_trigger")
        || lower == "call_trigger"
        || lower.ends_with("_trigger")
    {
        Some("scripted_trigger")
    } else if matches!(
        lower.as_str(),
        "localisation" | "localization" | "loc_key" | "name" | "desc" | "title" | "tooltip"
    ) {
        Some("localisation")
    } else {
        None
    }?;
    let scalar = property.scalar.as_ref()?;
    if scalar.value.is_empty()
        || scalar.value == "yes"
        || scalar.value == "no"
        || scalar.value.parse::<f64>().is_ok()
    {
        return None;
    }
    Some((kind.to_owned(), scalar.value.clone(), scalar.range))
}

fn definition_kind(path: &str, property: &HirProperty) -> Option<String> {
    if path.contains("scripted_effect") {
        return Some("scripted_effect".to_owned());
    }
    if path.contains("scripted_trigger") {
        return Some("scripted_trigger".to_owned());
    }
    if path.contains("events/") || property.key.ends_with("_event") {
        return Some("event".to_owned());
    }
    if property.value_range.is_some()
        && matches!(property.key.as_str(), "country_event" | "province_event")
    {
        return Some("event".to_owned());
    }
    if let Some(kind) = eu4_dynamic_definition_kind(path) {
        return Some(kind.to_owned());
    }
    None
}

fn eu4_dynamic_definition_kind(path: &str) -> Option<&'static str> {
    let path = path.trim_end_matches('/');
    if path.contains("common/country_tags") {
        return None;
    }
    let directory = path.rsplit_once('/').map_or(path, |(directory, _)| directory);
    Some(match directory {
        "common/cultures" => "culture",
        "common/religions" => "religion",
        "common/tradenodes" => "trade_node",
        "common/colonial_regions" => "colonial_region",
        "common/estates" => "estate",
        "common/ideas" => "idea_group",
        "common/governments" => "government",
        "common/government_reforms" => "government_reform",
        "common/subject_types" => "subject_type",
        "common/technologies" => "technology",
        "common/buildings" => "building",
        "common/units" => "unit_type",
        "common/mercenary_companies" => "mercenary_company",
        "common/trade_companies" => "trade_company",
        "common/advisortypes" => "advisor_type",
        "common/leader_personalities" => "leader_personality",
        "common/ruler_personalities" => "ruler_personality",
        "common/event_modifiers" => "event_modifier",
        "common/static_modifiers" => "static_modifier",
        "common/timed_modifiers" => "timed_modifier",
        "common/triggered_modifiers" => "triggered_modifier",
        "common/subject_type_upgrades" => "subject_type_upgrade",
        "common/peace_treaties" => "peace_treaty",
        "common/casus_belli" | "common/cb_types" => "casus_belli",
        "common/wargoal_types" => "wargoal_type",
        "common/institutions" => "institution",
        "common/great_projects" => "great_project",
        "common/estate_privileges" => "estate_privilege",
        "common/estate_agendas" => "estate_agenda",
        "common/diplomatic_actions" | "common/new_diplomatic_actions" => "diplomatic_action",
        "common/disasters" => "disaster",
        "common/rebel_types" => "rebel_type",
        "common/insults" => "insult",
        "common/opinion_modifiers" => "opinion_modifier",
        "common/tradegoods" => "tradegood",
        _ => return None,
    })
}

fn event_name(hir: &HirFile, property: &HirProperty) -> Option<String> {
    nested_hir_property(hir, property, "id")
        .and_then(|property| property.scalar.as_ref())
        .map(|scalar| scalar.value.clone())
}

fn nested_hir_property<'hir>(
    hir: &'hir HirFile,
    parent: &HirProperty,
    wanted: &str,
) -> Option<&'hir HirProperty> {
    hir.properties()
        .iter()
        .filter(|property| property.path.len() > parent.path.len())
        .filter(|property| property.path.starts_with(&parent.path))
        .filter(|property| range_within(property.range, parent.range))
        .find(|property| property.key.eq_ignore_ascii_case(wanted))
}

fn range_within(inner: TextRange, outer: TextRange) -> bool {
    inner.start() >= outer.start() && inner.end() <= outer.end()
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
    roots: Arc<[SourceRoot]>,
    workspace_root: Option<PathBuf>,
    documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    index: Arc<WorkspaceIndex>,
    scan_report: Arc<WorkspaceScanReport>,
}

impl AnalysisHost {
    /// Creates an empty host with the bootstrap EU4 rule identity.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(RuleSet::empty())
    }

    /// Creates an empty host around an immutable rule database.
    #[must_use]
    pub fn new(rules: RuleSet) -> Self {
        Self {
            revision: 0,
            rules: Arc::new(rules),
            roots: Arc::from([]),
            workspace_root: None,
            documents: Arc::new(BTreeMap::new()),
            source_files: Arc::new(BTreeMap::new()),
            file_states: Arc::new(BTreeMap::new()),
            index: Arc::new(WorkspaceIndex::empty()),
            scan_report: Arc::new(WorkspaceScanReport::default()),
        }
    }

    fn parser_for_document(&self, id: &DocumentId, path: Option<&Path>) -> Option<ParserKind> {
        let logical = path
            .and_then(|path| {
                self.roots
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
        if let Some(category) = logical.as_ref().and_then(|path| self.rules.classify(path)) {
            return Some(category.parser.clone());
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
        Some(match extension.as_str() {
            "yml" | "yaml" => ParserKind::Localisation,
            "csv" => ParserKind::Csv(pdx_rules::CsvDialect::Semicolon),
            "txt" | "gui" | "gfx" | "asset" | "sfx" => ParserKind::PdxScript,
            _ => return None,
        })
    }

    fn document_snapshot(
        &self,
        id: DocumentId,
        version: Option<i64>,
        text: String,
        source: DocumentSource,
        path: Option<PathBuf>,
    ) -> DocumentSnapshot {
        let line_index = LineIndex::new(&text);
        let (parsed, hir) = self
            .parser_for_document(&id, path.as_deref())
            .map_or((None, None), |parser| parse_source(&parser, &text, self.rules.as_ref()));
        DocumentSnapshot {
            id,
            version,
            text: Arc::from(text),
            line_index,
            source,
            path,
            parsed,
            hir,
        }
    }

    /// Applies one event-loop change and advances the snapshot revision.
    pub fn apply_change(&mut self, change: WorkspaceChange) {
        match change {
            WorkspaceChange::SetSourceRoots(roots) => self.roots = Arc::from(roots),
            WorkspaceChange::SetWorkspaceRoot(root) => self.workspace_root = root,
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Scans all configured roots in stable order and atomically refreshes source files and shards.
    pub fn refresh_source_roots(&mut self) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits(WorkspaceScanLimits::default())
    }

    /// Scans all configured roots with explicit resource limits.
    pub fn refresh_source_roots_with_limits(
        &mut self,
        limits: WorkspaceScanLimits,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        let mut files: BTreeMap<SourceFileId, SourceFile> = BTreeMap::new();
        let mut texts = BTreeMap::new();
        let mut report = WorkspaceScanReport::default();
        for root in self.roots.iter() {
            let mut paths = Vec::new();
            collect_disk_files(&root.path, &root.path, 0, limits, &mut report, &mut paths)?;
            paths.sort_by(|left, right| left.0.cmp(&right.0));
            for (logical, physical) in paths {
                let id = SourceFileId::new(stable_file_id(root.id, &logical));
                let Some(category) = self.rules.classify(&logical) else { continue };
                let Some(text) = read_source_file(&physical, limits, &mut report) else { continue };
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
                    Arc::new(build_file_state(file, text, file_revision, self.rules.as_ref()))
                }
            };
            file_states.insert(*id, state);
        }
        let shards = file_states.values().map(|state| state.shard().clone());
        let mut index = WorkspaceIndex::from_shards(shards);
        let priorities = source_priorities(&self.roots, &files);
        index.resolve_priorities(&priorities, self.rules.as_ref());
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

    /// Returns the immutable EU4 rules used for this snapshot.
    #[must_use]
    pub fn rules(&self) -> &RuleSet {
        &self.rules
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
        WorkspaceIndex, WorkspaceScanIssueKind, WorkspaceScanLimits,
    };
    use pdx_rules::RuleSet;
    use pdx_text::{LogicalPath, TextRange};

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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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
    fn snapshots_share_immutable_state_and_preserve_old_revisions() {
        let mut host = AnalysisHost::new(RuleSet::empty());
        let first = host.snapshot();
        let second = host.snapshot();

        assert!(Arc::ptr_eq(&first.rules, &second.rules));
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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

        let mut host = AnalysisHost::new(pdx_game_eu4::bootstrap_rules());
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
}
