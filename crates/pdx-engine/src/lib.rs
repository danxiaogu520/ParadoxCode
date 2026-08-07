//! Semantic engine: HIR lowering, workspace state, and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

#[cfg(test)]
use std::cell::Cell;
pub mod hir;

mod host;
mod index;
mod model;
mod pipeline;
mod scan;
mod snapshot;
mod vanilla_cache;

pub use host::AnalysisHost;
pub use index::{Definition, FileIndexShard, Reference, WorkspaceIndex};
pub use model::{
    DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId, DocumentSnapshot,
    DocumentSource, FileState, LocalisationPreview, ParsedSource, PreparedDocument,
    ResolvedCandidate, SourceFile, SourceFileId, SourceRoot, SourceRootId, SourceRootKind,
    TextChange, WorkspaceChange, WorkspaceError, WorkspaceScanIssue, WorkspaceScanIssueKind,
    WorkspaceScanLimits, WorkspaceScanReport, WorkspaceScanToken,
};
pub use snapshot::AnalysisSnapshot;
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

#[cfg(test)]
mod tests;
