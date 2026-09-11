//! Analysis host, immutable snapshots, and the query facade boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types. The underlying data model is provided by the
//! `vfs`, `hir`, and `index` crates and re-exported here as the stable facade.

mod host;
mod index_cache;
mod query_cache;
mod snapshot;

pub use host::AnalysisHost;
pub use query_cache::{CacheDomain, SnapshotQueryCache};
pub use snapshot::AnalysisSnapshot;

// Facade re-exports: downstream crates keep consuming these names through `engine::`.
pub use index::{
    Definition, DocumentSnapshot, DynamicDefinitionSummary, DynamicParameterSignature,
    FileIndexShard, FileState, FlagWrite, FlagWriteIndex, FlagWriteMembership,
    LocalisationPreviewMap, LocalisationPreviewMapIter, ParsedSource, PositionMap, PositionMapIter,
    PreparedDocument, Reference, WorkspaceIndex,
};
pub use index_cache::{
    CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError, IndexCacheMetadata,
};
pub use vfs::{
    CURRENT_PARSE_CACHE_SCHEMA_VERSION, DiskFileChange, DiskFileChangeKind, DocumentError,
    DocumentId, DocumentSource, LocalisationPreview, ParseCache, ParseCacheError,
    ResolvedCandidate, SourceFile, SourceFileId, SourceRoot, SourceRootId, SourceRootKind,
    StringPool, TextChange, WorkspaceChange, WorkspaceError, WorkspaceScanFilterError,
    WorkspaceScanFilters, WorkspaceScanIssue, WorkspaceScanIssueKind, WorkspaceScanLimits,
    WorkspaceScanReport, WorkspaceScanToken, intern_shard_string,
};

#[cfg(test)]
mod tests;
