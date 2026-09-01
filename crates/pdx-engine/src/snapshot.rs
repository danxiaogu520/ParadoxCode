//! Immutable workspace view used by analysis queries.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pdx_rules::{FileResolutionPolicy, GameProfile, RuleSet};
use pdx_text::{LogicalPath, TextRange};

use crate::index::{LocalisationPreviewMap, WorkspaceIndex};
use crate::model::{
    DocumentId, DocumentSnapshot, DocumentSource, FileState, LocalisationPreview, PreparedDocument,
    ResolvedCandidate, SourceFile, SourceFileId, SourceRoot, SourceRootKind, WorkspaceScanLimits,
    WorkspaceScanReport,
};
use crate::pipeline::prepare_document_snapshot;
use crate::query_cache::SnapshotQueryCache;
use crate::scan::root_priority;

/// Immutable workspace view used by analysis queries.
#[derive(Clone, Debug)]
pub struct AnalysisSnapshot {
    pub(crate) revision: u64,
    pub(crate) rules: Arc<RuleSet>,
    pub(crate) profile: Arc<GameProfile>,
    pub(crate) roots: Arc<[SourceRoot]>,
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    pub(crate) source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    pub(crate) source_file_paths: Arc<HashMap<PathBuf, SourceFileId>>,
    pub(crate) file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    pub(crate) index: Arc<WorkspaceIndex>,
    pub(crate) scan_report: Arc<WorkspaceScanReport>,
    pub(crate) localisation_previews: Arc<LocalisationPreviewMap>,
    pub(crate) query_cache: Arc<SnapshotQueryCache>,
    pub(crate) scan_limits: WorkspaceScanLimits,
    pub(crate) preferred_localisation_languages: Arc<[String]>,
    pub(crate) completion_source_layers: Arc<[SourceRootKind]>,
}

impl AnalysisSnapshot {
    /// Returns the monotonic revision captured by this snapshot.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the immutable game rules used for this snapshot.
    #[must_use]
    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// Returns the immutable game-specific interpretation selected for this snapshot.
    #[must_use]
    pub fn game_profile(&self) -> &GameProfile {
        &self.profile
    }

    /// Clones the shared game-profile handle without copying profile data.
    #[must_use]
    pub fn game_profile_handle(&self) -> Arc<GameProfile> {
        Arc::clone(&self.profile)
    }

    /// Returns source roots in configured order.
    #[must_use]
    pub fn source_roots(&self) -> &[SourceRoot] {
        &self.roots
    }

    /// Returns the explicit workspace root, if configured.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_root.as_deref()
    }

    /// Returns all current document candidates keyed by stable document identity.
    #[must_use]
    pub fn documents(&self) -> &BTreeMap<DocumentId, DocumentSnapshot> {
        &self.documents
    }

    /// Fully parses and lowers the exact staged overlay captured by this snapshot.
    #[must_use]
    pub fn prepare_document(&self, id: &DocumentId) -> Option<PreparedDocument> {
        let document = self.documents.get(id)?;
        if document.source != DocumentSource::Overlay {
            return None;
        }
        Some(PreparedDocument {
            document: prepare_document_snapshot(
                self.rules.as_ref(),
                self.profile.as_ref(),
                &self.roots,
                document.clone(),
            ),
        })
    }

    /// Returns one current document candidate.
    #[must_use]
    pub fn document(&self, id: &DocumentId) -> Option<&DocumentSnapshot> {
        self.documents.get(id)
    }

    /// Returns all discovered source files.
    #[must_use]
    pub fn source_files(&self) -> &BTreeMap<SourceFileId, SourceFile> {
        &self.source_files
    }

    /// Resolves the stable id of one scanned file by physical path.
    ///
    /// The map is maintained by the workspace scan and targeted disk-change pipelines, so the
    /// lookup is logarithmic instead of a linear scan over every indexed file (including
    /// cache-installed roots).
    #[must_use]
    pub fn source_file_id_for_path(&self, path: &std::path::Path) -> Option<SourceFileId> {
        self.source_file_paths.get(path).copied()
    }

    /// Returns the immutable parse/HIR/index state for one scanned disk file.
    #[must_use]
    pub fn file_state(&self, file_id: SourceFileId) -> Option<&FileState> {
        self.file_states.get(&file_id).map(Arc::as_ref)
    }

    /// Returns the immutable file/symbol index.
    #[must_use]
    pub fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Returns the workspace-wide localisation preview table (mostly Vanilla
    /// entries installed from the index cache).
    #[must_use]
    pub fn localisation_previews(&self) -> &LocalisationPreviewMap {
        &self.localisation_previews
    }

    /// Returns the bounded report from the latest successful source-root scan.
    #[must_use]
    pub fn scan_report(&self) -> &WorkspaceScanReport {
        &self.scan_report
    }

    /// Resolves one logical path, retaining lower-priority candidates as shadowed entries.
    #[must_use]
    pub fn resolve(&self, logical_path: &LogicalPath) -> Vec<ResolvedCandidate> {
        let mut candidates = self
            .source_files
            .values()
            .filter(|file| &file.logical_path == logical_path)
            .map(|file| {
                let priority = self
                    .roots
                    .iter()
                    .find(|root| root.id == file.root_id)
                    .map_or(0, root_priority);
                ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: Some(file.id),
                    document_id: None,
                    priority,
                    resolution: Some(file.resolution),
                    active: false,
                }
            })
            .collect::<Vec<_>>();
        for document in self.documents.values() {
            let Some(path) = document.path() else {
                continue;
            };
            let Some(root) = self.roots.iter().find(|root| path.starts_with(&root.path)) else {
                continue;
            };
            let Ok(relative) = path
                .strip_prefix(&root.path)
                .map(|value| LogicalPath::parse(&value.to_string_lossy()))
            else {
                continue;
            };
            if relative.as_ref().is_ok_and(|value| value == logical_path) {
                candidates.push(ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: None,
                    document_id: Some(document.id().clone()),
                    priority: 20_000,
                    resolution: None,
                    active: false,
                });
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority));
        let overlay_present = candidates
            .iter()
            .any(|candidate| candidate.document_id.is_some());
        if overlay_present {
            if let Some(first) = candidates.first_mut() {
                first.active = true;
            }
        } else if candidates
            .first()
            .is_some_and(|candidate| candidate.resolution == Some(FileResolutionPolicy::Merge))
        {
            for candidate in &mut candidates {
                candidate.active = true;
            }
        } else if let Some(first) = candidates.first_mut() {
            first.active = true;
        }
        candidates
    }

    /// Returns the current text for a disk file, if it was scanned.
    #[must_use]
    pub fn source_text(&self, file_id: SourceFileId) -> Option<&str> {
        self.file_state(file_id).map(FileState::source)
    }

    /// Returns the shared lazy query cache for this revision.
    ///
    /// Higher layers cache expensive snapshot-derived query results under `(revision, key)`;
    /// entries are immutable for the lifetime of the revision and are shared by every cloned
    /// snapshot value.
    #[must_use]
    pub fn query_cache(&self) -> &SnapshotQueryCache {
        &self.query_cache
    }

    /// Returns the bounded scan profile selected by the workspace configuration.
    #[must_use]
    pub const fn scan_limits(&self) -> WorkspaceScanLimits {
        self.scan_limits
    }

    /// Returns the preferred localisation language order. An empty list means the analysis
    /// default (English when available).
    #[must_use]
    pub fn preferred_localisation_languages(&self) -> &[String] {
        &self.preferred_localisation_languages
    }

    /// Returns the source layers eligible to contribute workspace completion members.
    #[must_use]
    pub fn completion_source_layers(&self) -> &[SourceRootKind] {
        &self.completion_source_layers
    }

    /// Returns whether one indexed source-root kind is enabled for completion members.
    #[must_use]
    pub fn completion_source_layer_enabled(&self, kind: SourceRootKind) -> bool {
        self.completion_source_layers.contains(&kind)
    }

    /// Returns a cached localisation preview without reading the source file.
    #[must_use]
    pub fn localisation_preview(
        &self,
        file_id: SourceFileId,
        range: TextRange,
    ) -> Option<&LocalisationPreview> {
        self.localisation_previews.get((file_id, range))
    }
}
