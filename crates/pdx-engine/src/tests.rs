use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

pub(crate) use super::{
    AnalysisHost, Definition, DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId,
    DocumentSource, FileIndexShard, MacroParameterSignature, ParsedSource, Reference, SourceFileId,
    SourceRoot, SourceRootId, SourceRootKind, TextChange, VanillaCacheError, VanillaIndexCache,
    WorkspaceChange, WorkspaceError, WorkspaceIndex, WorkspaceScanIssueKind, WorkspaceScanLimits,
    WorkspaceScanToken, pipeline_counts, reset_pipeline_counts,
};
use pdx_rules::{RuleSet, RulesModel, SymbolDescriptor, SymbolResolutionPolicy};
use pdx_text::{LogicalPath, TextRange};

fn eu4_host() -> AnalysisHost {
    AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile())
}

mod documents;
mod index;
mod scan;
mod vanilla;
