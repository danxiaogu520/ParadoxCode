use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

pub(crate) use super::{
    AnalysisHost, AnalysisSnapshot, Definition, DiskFileChange, DiskFileChangeKind, DocumentError,
    DocumentId, DocumentSource, DynamicParameterSignature, FileIndexShard, FlagWrite,
    FlagWriteMembership, IndexCache, IndexCacheError, LocalisationPreview, LocalisationPreviewMap,
    ParsedSource, PositionMap, Reference, SourceFileId, SourceRoot, SourceRootId, SourceRootKind,
    TextChange, TextureCatalog, WorkspaceChange, WorkspaceError, WorkspaceIndex,
    WorkspaceScanFilters, WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanToken,
};
use rules::{RuleSet, RulesModel, SymbolDescriptor, SymbolResolutionPolicy};
use text::{LogicalPath, Position, PositionRange, TextRange};

fn eu4_host() -> AnalysisHost {
    AnalysisHost::with_profile(game::eu4::bootstrap_rules(), game::eu4::profile())
}

fn eu4_host_with(rules: RuleSet) -> AnalysisHost {
    AnalysisHost::with_profile(rules, game::eu4::profile())
}

/// Creates an isolated fixture root under the system temp directory. Cleanup
/// stays with the caller (`fs::remove_dir_all`), matching the existing tests.
fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("engine-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("fixture root");
    root
}

mod documents;
mod index;
mod index_cache;
mod localisation;
mod scan;
mod texture;
