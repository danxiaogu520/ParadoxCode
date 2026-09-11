//! Source roots, workspace scans, and stable document data.
//!
//! The VFS layer owns file identity (source roots, files, documents), workspace scanning with
//! cancellation and limits, the on-disk parse cache, and the shared string pool. It knows
//! nothing about semantic analysis; higher layers consume these types read-only.

mod model;
mod parse_cache;
pub mod scan;
mod string_pool;

pub use model::localisation_previews_from_parsed;
pub use model::{
    DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId, DocumentSource,
    LocalisationPreview, ResolvedCandidate, SourceFile, SourceFileId, SourceRoot, SourceRootId,
    SourceRootKind, TextChange, WorkspaceChange, WorkspaceError, WorkspaceScanFilterError,
    WorkspaceScanFilters, WorkspaceScanIssue, WorkspaceScanIssueKind, WorkspaceScanLimits,
    WorkspaceScanReport, WorkspaceScanToken,
};
pub use parse_cache::{CURRENT_PARSE_CACHE_SCHEMA_VERSION, ParseCache, ParseCacheError};
pub use scan::{read_source_file_cancellable, root_priority};
pub use string_pool::{StringPool, intern_shard_string};
