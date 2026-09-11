//! Parsed, lowered, and indexed per-document analysis state.

use std::path::PathBuf;
use std::sync::Arc;

use hir::HirFile;
use parser::{FileFormat, ParsedFile};
use text::{LineIndex, TextRange};

use vfs::{DocumentId, DocumentSource, LocalisationPreview, localisation_previews_from_parsed};

use crate::index::FileIndexShard;

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
    pub revision: u64,
    pub source: Arc<str>,
    pub parsed: Option<ParsedSource>,
    pub hir: Option<Arc<HirFile>>,
    pub shard: Arc<FileIndexShard>,
    pub cached_localisation_previews: Option<Arc<Vec<(TextRange, LocalisationPreview)>>>,
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

    /// Returns the index shard shared with the workspace index.
    ///
    /// The shard lives behind an `Arc` that the workspace index shares, so
    /// installing or merging indexes never deep-copies every definition and
    /// reference string in the workspace.
    #[must_use]
    pub fn shard_handle(&self) -> Arc<FileIndexShard> {
        Arc::clone(&self.shard)
    }

    pub fn shard(&self) -> &FileIndexShard {
        &self.shard
    }

    pub fn cache_only(mut self) -> Self {
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
            cached_localisation_previews,
        }
    }

    pub fn cache_only_from_existing(&self) -> Self {
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
            cached_localisation_previews,
        }
    }

    pub fn cached_localisation_previews(&self) -> Option<&[(TextRange, LocalisationPreview)]> {
        self.cached_localisation_previews
            .as_deref()
            .map(Vec::as_slice)
    }

    /// Drops the retained CST/HIR frontends while keeping the source text,
    /// index shard, and any cached localisation previews.
    ///
    /// Used after background validation to bound resident memory: closed files
    /// rarely need their trees again, and pipeline callers can
    /// reparse the retained source on demand. Returns `None` when no frontend
    /// is retained. The revision is unchanged — eviction does not alter any
    /// answer, so snapshot query caches stay valid.
    pub fn evict_frontend(&self) -> Option<Self> {
        if self.parsed.is_none() && self.hir.is_none() {
            return None;
        }
        Some(Self {
            revision: self.revision,
            source: Arc::clone(&self.source),
            parsed: None,
            hir: None,
            shard: Arc::clone(&self.shard),
            cached_localisation_previews: self.cached_localisation_previews.clone(),
        })
    }
}

/// A document candidate exposed by an immutable workspace snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    pub version: Option<i64>,
    pub text: Arc<str>,
    pub line_index: LineIndex,
    pub source: DocumentSource,
    pub path: Option<PathBuf>,
    pub parsed: Option<ParsedSource>,
    pub hir: Option<Arc<HirFile>>,
}

/// A fully parsed overlay candidate prepared outside the mutable host.
#[derive(Clone, Debug)]
pub struct PreparedDocument {
    pub document: DocumentSnapshot,
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
