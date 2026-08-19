//! Stable workspace/source/document data and error models.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use pdx_parser::{CstKind, FileFormat, ParsedFile};
use pdx_rules::FileResolutionPolicy;
use pdx_text::{LineIndex, LogicalPath, PositionRange, TextRange};

use crate::hir::HirFile;
use crate::index::FileIndexShard;

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

/// Layer role of one source root.
///
/// Every layer is an ordered game-data source and overrides lower layers; the role is an
/// identity and display attribute only. Priority arithmetic never consults the kind: it is
/// derived exclusively from the globally unique [`SourceRoot::order`] assigned by the workspace
/// configuration (Vanilla 0, dependencies 1..n, Current Mod n+1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceRootKind {
    /// The base game-data layer (for EU4: the local Vanilla installation or its cache).
    Vanilla,
    /// An explicitly ordered dependency Mod layer.
    Dependency,
    /// The current Mod layer being edited, which also carries the workspace overlay.
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

    pub(crate) fn checkpoint(&self) -> Result<(), WorkspaceError> {
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
    pub(crate) fn cancel_after(checkpoints: usize) -> Self {
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
    /// A recoverable encoding span was replaced while retaining the surrounding source.
    EncodingRecovered,
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
    pub(crate) revision: u64,
    pub(crate) source: Arc<str>,
    pub(crate) parsed: Option<ParsedSource>,
    pub(crate) hir: Option<Arc<HirFile>>,
    pub(crate) shard: Arc<FileIndexShard>,
    pub(crate) cached_positions: Option<Arc<Vec<(TextRange, PositionRange)>>>,
    pub(crate) cached_localisation_previews: Option<Arc<Vec<(TextRange, LocalisationPreview)>>>,
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

    pub(crate) fn cache_only(mut self, positions: Vec<(TextRange, PositionRange)>) -> Self {
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

    pub(crate) fn cache_only_from_existing(
        &self,
        positions: Vec<(TextRange, PositionRange)>,
    ) -> Self {
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
    pub(crate) id: DocumentId,
    pub(crate) version: Option<i64>,
    pub(crate) text: Arc<str>,
    pub(crate) line_index: LineIndex,
    pub(crate) source: DocumentSource,
    pub(crate) path: Option<PathBuf>,
    pub(crate) parsed: Option<ParsedSource>,
    pub(crate) hir: Option<Arc<HirFile>>,
}

/// A fully parsed overlay candidate prepared outside the mutable host.
#[derive(Clone, Debug)]
pub struct PreparedDocument {
    pub(crate) document: DocumentSnapshot,
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
