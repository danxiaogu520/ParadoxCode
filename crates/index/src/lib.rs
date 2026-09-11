//! Workspace symbol index and the persistent `.pdcindex` cache.
//!
//! This layer lowers scanned files into per-file index shards, aggregates them into the
//! workspace index, and persists/loads whole-workspace `.pdcindex` caches. It also owns the
//! per-document analysis state (`FileState`, `DocumentSnapshot`) built by the pipeline.

use std::cell::Cell;

mod documents;
mod index;
mod pipeline;

pub use documents::{DocumentSnapshot, FileState, ParsedSource, PreparedDocument};
pub use index::{
    Definition, DynamicDefinitionSummary, DynamicParameterSignature, FileIndexShard, FlagWrite,
    FlagWriteIndex, FlagWriteMembership, LocalisationPreviewMap, LocalisationPreviewMapIter,
    PositionMap, PositionMapIter, Reference, WorkspaceIndex,
};
pub use pipeline::{
    SourceLoadContext, SourceReadJob, build_file_state, build_file_state_with_cache,
    empty_file_state, load_source_files, parse_source, position_ranges_for_state,
    prepare_document_snapshot, staged_overlay_document, unparsed_document,
};

thread_local! {
    static PIPELINE_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

#[doc(hidden)]
pub fn record_pipeline_parse() {
    PIPELINE_COUNTS.with(|counts| {
        let (parses, lowers) = counts.get();
        counts.set((parses.saturating_add(1), lowers));
    });
}

#[doc(hidden)]
pub fn record_pipeline_lower() {
    PIPELINE_COUNTS.with(|counts| {
        let (parses, lowers) = counts.get();
        counts.set((parses, lowers.saturating_add(1)));
    });
}

#[doc(hidden)]
pub fn reset_pipeline_counts() {
    PIPELINE_COUNTS.set((0, 0));
}

#[doc(hidden)]
pub fn pipeline_counts() -> (usize, usize) {
    PIPELINE_COUNTS.get()
}
