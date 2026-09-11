use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

pub(crate) use super::{
    AnalysisHost, AnalysisSnapshot, Definition, DiskFileChange, DiskFileChangeKind, DocumentError,
    DocumentId, DocumentSource, DynamicParameterSignature, FileIndexShard, FlagWrite,
    FlagWriteMembership, IndexCache, IndexCacheError, LocalisationPreview, LocalisationPreviewMap,
    ParsedSource, PositionMap, Reference, SourceFileId, SourceRoot, SourceRootId, SourceRootKind,
    TextChange, WorkspaceChange, WorkspaceError, WorkspaceIndex, WorkspaceScanFilters,
    WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanToken,
};
use rules::{RuleSet, RulesModel, SymbolDescriptor, SymbolResolutionPolicy};
use text::{LogicalPath, Position, PositionRange, TextRange};

fn eu4_host() -> AnalysisHost {
    AnalysisHost::with_profile(game::eu4::bootstrap_rules(), game::eu4::profile())
}

mod documents;
mod index;
mod index_cache;
mod localisation;
mod scan;
