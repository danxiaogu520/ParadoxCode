//! Semantic engine: HIR lowering, workspace state, and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

#[cfg(test)]
use std::cell::Cell;
pub mod hir;

mod host;
mod index;
mod index_cache;
mod model;
mod parse_cache;
mod pipeline;
mod query_cache;
mod scan;
mod snapshot;
mod string_pool;

pub use host::AnalysisHost;
pub use index::{
    Definition, FileIndexShard, FlagWrite, FlagWriteIndex, FlagWriteMembership,
    LocalisationPreviewMap, LocalisationPreviewMapIter, MacroDefinitionSummary,
    MacroParameterSignature, PositionMap, PositionMapIter, Reference, WorkspaceIndex,
};
pub use index_cache::{
    CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError, IndexCacheMetadata,
};
pub use model::{
    DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId, DocumentSnapshot,
    DocumentSource, FileState, LocalisationPreview, ParsedSource, PreparedDocument,
    ResolvedCandidate, SourceFile, SourceFileId, SourceRoot, SourceRootId, SourceRootKind,
    TextChange, WorkspaceChange, WorkspaceError, WorkspaceScanFilterError, WorkspaceScanFilters,
    WorkspaceScanIssue, WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanReport,
    WorkspaceScanToken,
};
pub use parse_cache::{CURRENT_PARSE_CACHE_SCHEMA_VERSION, ParseCache, ParseCacheError};
pub use query_cache::{CacheDomain, SnapshotQueryCache};
pub use snapshot::AnalysisSnapshot;
pub use string_pool::{StringPool, intern_shard_string};

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

#[cfg(test)]
mod tests;
