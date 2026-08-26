use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

pub(crate) use super::{
    AnalysisHost, Definition, DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId,
    DocumentSource, FileIndexShard, IndexCache, IndexCacheError, LocalisationPreview,
    LocalisationPreviewMap, MacroParameterSignature, ParsedSource, PositionMap, Reference,
    SourceFileId, SourceRoot, SourceRootId, SourceRootKind, TextChange, WorkspaceChange,
    WorkspaceError, WorkspaceIndex, WorkspaceScanIssueKind, WorkspaceScanLimits,
    WorkspaceScanToken, pipeline_counts, reset_pipeline_counts,
};
use pdx_rules::{RuleSet, RulesModel, SymbolDescriptor, SymbolResolutionPolicy};
use pdx_text::{LogicalPath, Position, PositionRange, TextRange};

fn eu4_host() -> AnalysisHost {
    AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile())
}

mod documents;
mod index;
mod index_cache;
mod scan;
