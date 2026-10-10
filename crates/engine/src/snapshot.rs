//! Immutable workspace view used by analysis queries.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use rules::ir::RulesIr;
use rules::{GameProfile, RuleSet};
use text::{AbsPath, LogicalPath, TextRange};

use crate::query_cache::SnapshotQueryCache;
use index::prepare_document_snapshot_with_ir;
use index::{DocumentSnapshot, FileState, PreparedDocument};
use index::{LocalisationPreviewMap, Reference, WorkspaceIndex};
use vfs::{
    DocumentId, DocumentSource, LocalisationPreview, ResolvedCandidate, SourceFile, SourceFileId,
    SourceRoot, SourceRootKind, WorkspaceScanLimits, WorkspaceScanReport,
};

/// The single localisation language whose previews analysis retains and hover
/// displays: the first configured preference, defaulting to English.
pub fn localisation_preview_target_language(preferred: &[String]) -> &str {
    preferred.first().map_or("english", String::as_str)
}

/// Immutable workspace view used by analysis queries.
#[derive(Clone, Debug)]
pub struct AnalysisSnapshot {
    pub(crate) revision: u64,
    pub(crate) rules: Arc<RuleSet>,
    pub(crate) ir: Arc<RulesIr>,
    pub(crate) ir_fingerprint: Arc<str>,
    pub(crate) profile: Arc<GameProfile>,
    pub(crate) roots: Arc<[SourceRoot]>,
    pub(crate) workspace_root: Option<AbsPath>,
    pub(crate) documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    pub(crate) source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    pub(crate) source_file_paths: Arc<HashMap<AbsPath, SourceFileId>>,
    pub(crate) file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    pub(crate) index: Arc<WorkspaceIndex>,
    pub(crate) scan_report: Arc<WorkspaceScanReport>,
    pub(crate) localisation_previews: Arc<LocalisationPreviewMap>,
    pub(crate) query_cache: Arc<SnapshotQueryCache>,
    pub(crate) scan_limits: WorkspaceScanLimits,
    pub(crate) scan_filters: Arc<vfs::WorkspaceScanFilters>,
    pub(crate) preferred_localisation_languages: Arc<[String]>,
    pub(crate) completion_source_layers: Arc<[SourceRootKind]>,
    pub(crate) texture_catalog: Arc<crate::texture::TextureCatalog>,
    /// Invalidation generation of the texture catalog at snapshot build time;
    /// see [`AnalysisSnapshot::texture_catalog_generation`].
    pub(crate) texture_catalog_generation: u64,
    /// Persistent parse cache, when configured; see
    /// [`AnalysisSnapshot::parse_cache`].
    pub(crate) parse_cache: Option<vfs::ParseCache>,
    /// Lazy symbol-reference stores of installed caches; see
    /// [`AnalysisSnapshot::lazy_references_for`].
    pub(crate) reference_sources: Arc<
        std::collections::BTreeMap<vfs::SourceRootId, Arc<crate::index_cache::ReferenceIndexStore>>,
    >,
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

    /// Returns the immutable rules-v2 IR used for this snapshot.
    ///
    /// The composition root installs the checked, embedded IR. Consumers read
    /// the same immutable semantic declarations for this snapshot.
    #[must_use]
    pub fn ir(&self) -> &RulesIr {
        &self.ir
    }

    /// Resolves a declared localisation template from the active IR.
    #[must_use]
    pub fn localisation_template_key(
        &self,
        type_name: &str,
        binding_name: &str,
        instance: &str,
    ) -> Option<String> {
        self.ir
            .localisation_template_key(type_name, binding_name, instance)
    }

    /// Shared immutable arena for background index workers.
    #[must_use]
    pub fn ir_handle(&self) -> Arc<RulesIr> {
        Arc::clone(&self.ir)
    }

    /// Stable content identity of this snapshot's immutable arena.
    #[must_use]
    pub fn ir_fingerprint(&self) -> &str {
        &self.ir_fingerprint
    }

    /// Texture-catalog invalidation generation captured by this snapshot.
    /// Distinct values mean the workspace asset catalog was rebuilt in
    /// between, which workspace-context fingerprints must treat as a
    /// diagnostics input change.
    #[must_use]
    pub const fn texture_catalog_generation(&self) -> u64 {
        self.texture_catalog_generation
    }

    /// The persistent parse cache, when the host was configured with one.
    /// Transient reparses consult it first: loading a validated CST from
    /// disk is ~9x cheaper than reparsing (see the `parse_cache_speed`
    /// example), and entries are checked against the live source hash.
    #[must_use]
    pub fn parse_cache(&self) -> Option<&vfs::ParseCache> {
        self.parse_cache.as_ref()
    }

    /// References for one `(kind, name)` pair served lazily from installed
    /// cache files — the kinds that were skipped at load time. Concatenating
    /// the per-store vectors mirrors what a fully materialized index would
    /// have answered; callers keep their own ordering/dedup rules.
    pub fn lazy_references_for(&self, kind: &str, name: &str) -> Vec<(SourceFileId, Reference)> {
        let mut references = Vec::new();
        for store in self.reference_sources.values() {
            references.extend(store.references_for(kind, name).iter().cloned());
        }
        references
    }

    /// Backing reference stores that no longer represent the installed semantic snapshot.
    /// Existing query contracts remain unchanged; clients requiring completeness can report these.
    #[must_use]
    pub fn reference_source_issues(&self) -> Vec<(vfs::SourceRootId, String)> {
        self.reference_sources
            .iter()
            .filter_map(|(root, store)| store.availability_issue().map(|issue| (*root, issue)))
            .collect()
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

    /// Returns the workspace asset catalog backing `texture_path` matchers.
    #[must_use]
    pub fn texture_catalog(&self) -> &crate::texture::TextureCatalog {
        &self.texture_catalog
    }

    /// Resolves one raw `.gfx` asset-path value against the workspace catalog.
    #[must_use]
    pub fn resolve_texture_path(&self, raw: &str) -> Option<crate::texture::TextureResolution> {
        self.texture_catalog.resolve(&self.roots, raw)
    }

    /// Returns the explicit workspace root, if configured.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&AbsPath> {
        self.workspace_root.as_ref()
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
            document: prepare_document_snapshot_with_ir(
                self.rules.as_ref(),
                self.profile.as_ref(),
                self.ir.as_ref(),
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

    /// Shares the immutable file catalogue; its identity survives unrelated document edits.
    #[must_use]
    pub fn source_files_handle(&self) -> Arc<BTreeMap<SourceFileId, SourceFile>> {
        Arc::clone(&self.source_files)
    }

    /// Resolves the stable id of one scanned file by physical path.
    ///
    /// The map is maintained by the workspace scan and targeted disk-change pipelines, so the
    /// lookup is logarithmic instead of a linear scan over every indexed file (including
    /// cache-installed roots).
    #[must_use]
    pub fn source_file_id_for_path(&self, path: &std::path::Path) -> Option<SourceFileId> {
        self.source_file_paths
            .get(&AbsPath::normalize(path))
            .copied()
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
        self.resolution_candidates(logical_path)
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

    /// Returns the identity of the analysis state that produced this snapshot.
    ///
    /// Snapshots share an id if and only if they come from the same `AnalysisHost`;
    /// revision numbers alone are only unique within one host. Thread-local fast
    /// paths keyed by revision must include this id so a second host reaching the
    /// same revision cannot observe the first host's cached views.
    #[must_use]
    pub fn host_identity(&self) -> u64 {
        self.query_cache.id()
    }

    /// Returns the bounded scan profile selected by the workspace configuration.
    #[must_use]
    pub const fn scan_limits(&self) -> WorkspaceScanLimits {
        self.scan_limits
    }

    /// Source-root exclusions shared by discovery and read-only text searches.
    #[must_use]
    pub fn scan_filters(&self) -> &vfs::WorkspaceScanFilters {
        &self.scan_filters
    }

    /// Returns the preferred localisation language order. An empty list means the analysis
    /// default (English when available).
    #[must_use]
    pub fn preferred_localisation_languages(&self) -> &[String] {
        &self.preferred_localisation_languages
    }

    /// Returns the localisation language hover previews display and the
    /// retained preview maps keep: the first configured preference, or
    /// English when none is configured.
    #[must_use]
    pub fn localisation_preview_language(&self) -> &str {
        localisation_preview_target_language(&self.preferred_localisation_languages)
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
