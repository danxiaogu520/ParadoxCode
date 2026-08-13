//! Mutable workspace owner and atomic state transitions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdx_rules::{GameProfile, ParserKind, RuleSet};
use pdx_text::{LogicalPath, TextRange};

use crate::index::{FileIndexShard, WorkspaceIndex};
use crate::model::{
    DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId, DocumentSnapshot,
    DocumentSource, FileState, LocalisationPreview, PreparedDocument, SourceFile, SourceFileId,
    SourceRoot, SourceRootId, SourceRootKind, TextChange, WorkspaceChange, WorkspaceError,
    WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanReport, WorkspaceScanToken,
};
use crate::pipeline::{
    SourceLoadContext, SourceReadJob, build_file_state, empty_file_state, load_source_files,
    position_ranges_for_state, prepare_document_snapshot, staged_overlay_document,
    unparsed_document,
};
use crate::query_cache::SnapshotQueryCache;
use crate::scan::{
    collect_whitelisted_files, read_source_file, read_source_file_cancellable, record_scan_issue,
    source_priorities, stable_file_id,
};
use crate::snapshot::AnalysisSnapshot;
use crate::vanilla_cache::{VanillaCacheError, VanillaIndexCache};

/// Mutable owner of workspace state.
#[derive(Clone, Debug)]
pub struct AnalysisHost {
    revision: u64,
    rules: Arc<RuleSet>,
    profile: Arc<GameProfile>,
    roots: Arc<[SourceRoot]>,
    workspace_root: Option<PathBuf>,
    documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    source_file_paths: Arc<HashMap<PathBuf, SourceFileId>>,
    file_states: Arc<BTreeMap<SourceFileId, Arc<FileState>>>,
    index: Arc<WorkspaceIndex>,
    scan_report: Arc<WorkspaceScanReport>,
    vanilla_root: Option<SourceRoot>,
    installed_caches: BTreeSet<SourceRootId>,
    vanilla_localisation_previews: Arc<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>>,
    query_cache: Arc<SnapshotQueryCache>,
}

impl AnalysisHost {
    /// Creates an empty host with no game-specific rule identity.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(RuleSet::empty())
    }

    /// Creates an empty host around an immutable rule database.
    #[must_use]
    pub fn new(rules: RuleSet) -> Self {
        let profile = GameProfile::empty(rules.game_id());
        Self::with_profile(rules, profile)
    }

    /// Creates an empty host with explicit game-specific profile data.
    #[must_use]
    pub fn with_profile(rules: RuleSet, profile: GameProfile) -> Self {
        Self {
            revision: 0,
            rules: Arc::new(rules),
            profile: Arc::new(profile),
            roots: Arc::from([]),
            workspace_root: None,
            documents: Arc::new(BTreeMap::new()),
            source_files: Arc::new(BTreeMap::new()),
            source_file_paths: Arc::new(HashMap::new()),
            file_states: Arc::new(BTreeMap::new()),
            index: Arc::new(WorkspaceIndex::empty()),
            scan_report: Arc::new(WorkspaceScanReport::default()),
            vanilla_root: None,
            installed_caches: BTreeSet::new(),
            vanilla_localisation_previews: Arc::new(BTreeMap::new()),
            query_cache: Arc::new(SnapshotQueryCache::new()),
        }
    }

    fn document_snapshot(
        &self,
        id: DocumentId,
        version: Option<i64>,
        text: String,
        source: DocumentSource,
        path: Option<PathBuf>,
    ) -> DocumentSnapshot {
        prepare_document_snapshot(
            self.rules.as_ref(),
            self.profile.as_ref(),
            &self.roots,
            unparsed_document(id, version, text, source, path),
        )
    }

    /// Applies one event-loop change and advances the snapshot revision.
    pub fn apply_change(&mut self, change: WorkspaceChange) {
        match change {
            WorkspaceChange::SetSourceRoots(roots) => {
                self.roots = Arc::from(roots);
                self.vanilla_root = None;
                self.installed_caches = BTreeSet::new();
                self.vanilla_localisation_previews = Arc::new(BTreeMap::new());
            }
            WorkspaceChange::SetWorkspaceRoot(root) => self.workspace_root = root,
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Installs a validated persistent Vanilla cache without scanning its original directory.
    ///
    /// The cache's rule hash is intentionally not required to match. It remains observable in
    /// metadata so the user can decide whether to run an explicit refresh.
    pub fn install_vanilla_cache(
        &mut self,
        cache: VanillaIndexCache,
    ) -> Result<(), VanillaCacheError> {
        let vanilla = cache.source_root().clone();
        self.install_index_cache(cache)?;
        self.vanilla_root = Some(vanilla);
        Ok(())
    }

    /// Installs a validated persistent index cache for any configured source root.
    ///
    /// The cached root may already be configured (a dependency with an explicit index): its
    /// identity must then match the configured root and the cache replaces live scanning for it.
    /// An unknown cached root (the Vanilla installation) is inserted at the front of the root
    /// order. The cache's rule hash is intentionally not required to match; it remains observable
    /// in metadata so the caller can decide whether to refresh.
    pub fn install_index_cache(
        &mut self,
        cache: VanillaIndexCache,
    ) -> Result<(), VanillaCacheError> {
        if cache.metadata().game_id != self.rules.game_id()
            || cache.metadata().game_id != self.profile.game_id
        {
            return Err(VanillaCacheError::GameMismatch {
                expected: self.profile.game_id.clone(),
                actual: cache.metadata().game_id.clone(),
            });
        }
        let cached_root = cache.source_root().clone();
        let mut roots = self.roots.to_vec();
        match roots.iter_mut().find(|root| root.id == cached_root.id) {
            Some(configured) => {
                if configured.kind != cached_root.kind {
                    return Err(VanillaCacheError::InvalidData(format!(
                        "cached root kind {:?} does not match configured root {} for id {}",
                        cached_root.kind,
                        configured.path.display(),
                        cached_root.id.get()
                    )));
                }
                if !paths_match(&configured.path, &cached_root.path) {
                    return Err(VanillaCacheError::InvalidData(format!(
                        "cached root path {} does not match configured root {}",
                        cached_root.path.display(),
                        configured.path.display()
                    )));
                }
                // The configured root keeps its caller-assigned order; only its index data
                // is replaced.
            }
            None => {
                for root in roots.iter() {
                    if root.kind == cached_root.kind {
                        return Err(VanillaCacheError::InvalidData(format!(
                            "a {:?} source root is already configured",
                            cached_root.kind
                        )));
                    }
                    if cached_root.path.starts_with(&root.path)
                        || root.path.starts_with(&cached_root.path)
                    {
                        return Err(VanillaCacheError::RootConflict {
                            vanilla: cached_root.path.clone(),
                            configured: root.path.clone(),
                        });
                    }
                }
                roots.insert(0, cached_root.clone());
            }
        }
        if let Some(file) = cache
            .source_files()
            .values()
            .find(|file| !self.profile.allows_scan_file(file.logical_path.as_str()))
        {
            return Err(VanillaCacheError::InvalidData(format!(
                "cache file {} is outside the active profile scan whitelist",
                file.logical_path.as_str()
            )));
        }

        let (_, _, mut files, cached_index, cached_positions, cached_previews) = cache.into_parts();
        for (id, file) in self.source_files.iter() {
            if let Some(cached) = files.insert(*id, file.clone()) {
                return Err(VanillaCacheError::InvalidData(format!(
                    "file id collision between {} and {}",
                    cached.physical_path.display(),
                    file.physical_path.display()
                )));
            }
        }
        let mut shards = cached_index.shards.into_values().collect::<Vec<_>>();
        shards.extend(self.index.shards.values().cloned());
        // One combined build sets the case policy and derives the lookup maps together, so the
        // merged cache + workspace shards are not rebuilt twice.
        let mut index = WorkspaceIndex::from_shards_with_rules(shards, self.rules.as_ref());
        index.replace_all_position_ranges(cached_positions);
        for (file_id, state) in self.file_states.iter() {
            index.replace_position_ranges(*file_id, position_ranges_for_state(state));
        }
        let priorities = source_priorities(&roots, &files);
        index.resolve_priorities(&priorities, self.rules.as_ref());

        let mut localisation_previews = (*self.vanilla_localisation_previews).clone();
        localisation_previews.extend(cached_previews);
        self.roots = Arc::from(roots);
        self.source_files = Arc::new(files);
        self.source_file_paths = Arc::new(source_file_paths(&self.source_files));
        self.index = Arc::new(index);
        self.vanilla_localisation_previews = Arc::new(localisation_previews);
        self.installed_caches.insert(cached_root.id);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Scans all configured roots in stable order and atomically refreshes source files and shards.
    pub fn refresh_source_roots(&mut self) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits(WorkspaceScanLimits::default())
    }

    /// Scans configured roots while cooperatively observing `cancellation`.
    pub fn refresh_source_roots_cancellable(
        &mut self,
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            WorkspaceScanLimits::default(),
            cancellation,
            None,
        )
    }

    /// Scans configured roots while cooperatively observing `cancellation`, invoking `progress`
    /// with `(completed, total)` source files so long-running background work can be surfaced.
    pub fn refresh_source_roots_cancellable_with_progress(
        &mut self,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            WorkspaceScanLimits::default(),
            cancellation,
            progress,
        )
    }

    /// Scans all configured roots with explicit resource limits.
    pub fn refresh_source_roots_with_limits(
        &mut self,
        limits: WorkspaceScanLimits,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            limits,
            &WorkspaceScanToken::new(),
            None,
        )
    }

    /// Scans configured roots with explicit resource limits and cooperative cancellation.
    pub fn refresh_source_roots_with_limits_and_cancellation(
        &mut self,
        limits: WorkspaceScanLimits,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        cancellation.checkpoint()?;
        let mut files: BTreeMap<SourceFileId, SourceFile> = BTreeMap::new();
        let mut file_states: BTreeMap<SourceFileId, Arc<FileState>> = BTreeMap::new();
        let mut source_jobs = Vec::new();
        let mut report = WorkspaceScanReport::default();
        for root in self.roots.iter() {
            cancellation.checkpoint()?;
            if self.installed_caches.contains(&root.id) {
                continue;
            }
            let mut paths = Vec::new();
            collect_whitelisted_files(
                &root.path,
                self.profile.as_ref(),
                limits,
                &mut report,
                &mut paths,
                cancellation,
            )?;
            paths.sort_by(|left, right| left.0.cmp(&right.0));
            for (logical, physical) in paths {
                cancellation.checkpoint()?;
                let id = SourceFileId::new(stable_file_id(root.id, &logical));
                let Some(category) = self.rules.classify(&logical) else {
                    continue;
                };
                let source_file = SourceFile {
                    id,
                    root_id: root.id,
                    physical_path: physical.clone(),
                    logical_path: logical,
                    category_id: Some(category.id.clone()),
                    resolution: category.resolution,
                };
                // Opaque resources participate in path/overlay resolution, but have no
                // text parser or semantic state. In particular, do not read binary assets as
                // UTF-8 just to manufacture an empty shard for them.
                if matches!(&category.parser, ParserKind::Asset) {
                    let file_for_state = source_file.clone();
                    if let Some(existing) = files.insert(id, source_file) {
                        return Err(WorkspaceError::FileIdCollision {
                            first: existing.physical_path,
                            second: physical,
                        });
                    }
                    let file_revision = self
                        .file_states
                        .get(&id)
                        .map_or(0, |state| state.revision().saturating_add(1));
                    let state = match self.file_states.get(&id) {
                        Some(previous)
                            if self.source_files.get(&id) == Some(&file_for_state)
                                && previous.source().is_empty() =>
                        {
                            Arc::clone(previous)
                        }
                        _ => Arc::new(empty_file_state(&file_for_state, file_revision)),
                    };
                    file_states.insert(id, state);
                    report.indexed_files = report.indexed_files.saturating_add(1);
                    continue;
                }
                source_jobs.push(SourceReadJob {
                    file: source_file,
                    physical_path: physical,
                    retain_frontend: root.kind != SourceRootKind::Vanilla,
                });
            }
        }
        let source_context = SourceLoadContext {
            limits,
            previous_files: self.source_files.as_ref(),
            previous_states: self.file_states.as_ref(),
            rules: self.rules.as_ref(),
            profile: self.profile.as_ref(),
            cancellation,
            progress,
        };
        load_source_files(
            source_jobs,
            &mut files,
            &mut file_states,
            &mut report,
            &source_context,
        )?;
        cancellation.checkpoint()?;
        let mut shards = file_states
            .values()
            .map(|state| state.shard().clone())
            .collect::<Vec<_>>();
        if !self.installed_caches.is_empty() {
            for (id, cached) in self
                .source_files
                .iter()
                .filter(|(_, file)| self.installed_caches.contains(&file.root_id))
            {
                if let Some(existing) = files.insert(*id, cached.clone()) {
                    return Err(WorkspaceError::FileIdCollision {
                        first: existing.physical_path,
                        second: cached.physical_path.clone(),
                    });
                }
            }
            shards.extend(
                self.index
                    .shards
                    .iter()
                    .filter(|(id, _)| {
                        self.source_files
                            .get(id)
                            .is_some_and(|file| self.installed_caches.contains(&file.root_id))
                    })
                    .map(|(_, shard)| shard.clone()),
            );
        }
        let mut index = WorkspaceIndex::from_shards_cancellable_with_rules(
            shards,
            self.rules.as_ref(),
            cancellation,
        )?;
        let mut position_ranges = self.index.position_ranges().clone();
        position_ranges.retain(|(file_id, _), _| {
            files.contains_key(file_id) && !file_states.contains_key(file_id)
        });
        for (file_id, state) in &file_states {
            position_ranges.extend(
                position_ranges_for_state(state)
                    .into_iter()
                    .map(|(range, position)| ((*file_id, range), position)),
            );
        }
        index.replace_all_position_ranges(position_ranges);
        let priorities = source_priorities(&self.roots, &files);
        let has_multiple_source_roots = self
            .roots
            .iter()
            .filter(|root| files.values().any(|file| file.root_id == root.id))
            .nth(1)
            .is_some();
        if !self.installed_caches.is_empty() || has_multiple_source_roots {
            index.resolve_priorities_cancellable(&priorities, self.rules.as_ref(), cancellation)?;
        }
        cancellation.checkpoint()?;
        self.source_files = Arc::new(files);
        self.source_file_paths = Arc::new(source_file_paths(&self.source_files));
        self.file_states = Arc::new(file_states);
        self.index = Arc::new(index);
        self.scan_report = Arc::new(report.clone());
        self.revision = self.revision.saturating_add(1);
        Ok(report)
    }

    /// Applies a batch of Current Mod/Dependency disk events with one atomic snapshot commit.
    ///
    /// Only changed file states and their index shards are rebuilt. Open overlays remain intact,
    /// and a persistent Vanilla root is never read or watched through this path.
    pub fn apply_disk_file_changes(
        &mut self,
        changes: &[DiskFileChange],
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.apply_disk_file_changes_cancellable(changes, &WorkspaceScanToken::new())
    }

    /// Applies targeted disk events while cooperatively observing `cancellation`.
    pub fn apply_disk_file_changes_cancellable(
        &mut self,
        changes: &[DiskFileChange],
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        cancellation.checkpoint()?;
        let limits = WorkspaceScanLimits::default();
        let mut files = self.source_files.as_ref().clone();
        let mut paths = self.source_file_paths.as_ref().clone();
        let mut file_states = self.file_states.as_ref().clone();
        let mut index = self.index.as_ref().clone();
        let mut report = WorkspaceScanReport::default();
        let mut changed = false;

        for change in changes {
            cancellation.checkpoint()?;
            let Some(root) = self
                .roots
                .iter()
                .filter(|root| {
                    matches!(
                        root.kind,
                        SourceRootKind::CurrentMod | SourceRootKind::Dependency
                    )
                })
                .filter(|root| change.path.starts_with(&root.path))
                .max_by_key(|root| root.path.as_os_str().len())
            else {
                continue;
            };
            let relative = change
                .path
                .strip_prefix(&root.path)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(change.path.clone()))?
                .to_string_lossy()
                .replace('\\', "/");
            let logical = LogicalPath::parse(&relative)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(change.path.clone()))?;
            if !self.profile.allows_scan_file(logical.as_str()) {
                continue;
            }
            let id = SourceFileId::new(stable_file_id(root.id, &logical));
            report.discovered_files = report.discovered_files.saturating_add(1);

            let missing = match fs::symlink_metadata(&change.path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    record_scan_issue(
                        &mut report,
                        limits,
                        WorkspaceScanIssueKind::SymlinkSkipped,
                        change.path.clone(),
                        "symbolic links are not followed during targeted disk updates".to_owned(),
                    );
                    continue;
                }
                Ok(metadata) => !metadata.is_file(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    record_scan_issue(
                        &mut report,
                        limits,
                        WorkspaceScanIssueKind::MetadataUnreadable,
                        change.path.clone(),
                        error.to_string(),
                    );
                    continue;
                }
            };
            if change.kind == DiskFileChangeKind::Deleted || missing {
                if files.remove(&id).is_some() {
                    paths.remove(&change.path);
                    file_states.remove(&id);
                    let priorities = source_priorities(&self.roots, &files);
                    index.remove_shard_resolved(id, &priorities, self.rules.as_ref());
                    index.remove_position_ranges(id);
                    changed = true;
                }
                continue;
            }

            let Some(category) = self.rules.classify(&logical) else {
                continue;
            };
            let source_file = SourceFile {
                id,
                root_id: root.id,
                physical_path: change.path.clone(),
                logical_path: logical,
                category_id: Some(category.id.clone()),
                resolution: category.resolution,
            };
            if let Some(existing) = files.get(&id)
                && existing.physical_path != source_file.physical_path
            {
                return Err(WorkspaceError::FileIdCollision {
                    first: existing.physical_path.clone(),
                    second: source_file.physical_path,
                });
            }
            let text = if matches!(&category.parser, ParserKind::Asset) {
                None
            } else {
                let Some(text) = read_source_file_cancellable(
                    &change.path,
                    limits,
                    &mut report,
                    cancellation,
                    self.profile.source_encoding,
                )?
                else {
                    continue;
                };
                Some(text)
            };
            if files.get(&id) == Some(&source_file)
                && file_states
                    .get(&id)
                    .is_some_and(|state| text.as_deref().is_none_or(|text| state.source() == text))
            {
                continue;
            }
            let file_revision = file_states
                .get(&id)
                .map_or(0, |state| state.revision().saturating_add(1));
            let state = Arc::new(match text {
                Some(text) => build_file_state(
                    &source_file,
                    text,
                    file_revision,
                    self.rules.as_ref(),
                    self.profile.as_ref(),
                ),
                None => empty_file_state(&source_file, file_revision),
            });
            files.insert(id, source_file);
            paths.insert(change.path.clone(), id);
            file_states.insert(id, Arc::clone(&state));
            let priorities = source_priorities(&self.roots, &files);
            index.replace_shard_resolved(state.shard().clone(), &priorities, self.rules.as_ref());
            index.replace_position_ranges(id, position_ranges_for_state(&state));
            report.indexed_files = report.indexed_files.saturating_add(1);
            changed = true;
        }

        cancellation.checkpoint()?;
        if changed {
            self.source_files = Arc::new(files);
            self.source_file_paths = Arc::new(paths);
            self.file_states = Arc::new(file_states);
            self.index = Arc::new(index);
            self.scan_report = Arc::new(report.clone());
            self.revision = self.revision.saturating_add(1);
        }
        Ok(report)
    }

    /// Returns a mutable workspace index for targeted shard replacement.
    pub fn replace_index_shard(&mut self, shard: FileIndexShard) {
        let file_id = shard.file_id;
        let priorities = source_priorities(&self.roots, &self.source_files);
        Arc::make_mut(&mut self.index).replace_shard_resolved(
            shard.clone(),
            &priorities,
            self.rules.as_ref(),
        );
        Arc::make_mut(&mut self.index).remove_position_ranges(file_id);
        if let Some(previous) = self.file_states.get(&file_id) {
            let mut replacement = previous.as_ref().clone();
            replacement.shard = Arc::new(shard);
            Arc::make_mut(&mut self.index)
                .replace_position_ranges(file_id, position_ranges_for_state(&replacement));
            Arc::make_mut(&mut self.file_states).insert(file_id, Arc::new(replacement));
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Opens a document overlay with a complete initial text.
    pub fn open_document(
        &mut self,
        id: DocumentId,
        version: i64,
        text: String,
        path: Option<PathBuf>,
    ) -> Result<(), DocumentError> {
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.source == DocumentSource::Overlay)
        {
            return Err(DocumentError::AlreadyOpen(id));
        }
        let document = self.document_snapshot(
            id.clone(),
            Some(version),
            text,
            DocumentSource::Overlay,
            path,
        );
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Stages the latest overlay text without parsing it.
    ///
    /// A worker can call [`AnalysisSnapshot::prepare_document`] on the resulting snapshot and
    /// return the candidate to [`Self::commit_prepared_document`].
    pub fn stage_open_document(
        &mut self,
        id: DocumentId,
        version: i64,
        text: String,
        path: Option<PathBuf>,
    ) -> Result<(), DocumentError> {
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.source == DocumentSource::Overlay)
        {
            return Err(DocumentError::AlreadyOpen(id));
        }
        let document = staged_overlay_document(id.clone(), version, text, path);
        Arc::make_mut(&mut self.documents).insert(id, document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Stages a complete newer overlay text without parsing it.
    pub fn stage_document_text(
        &mut self,
        id: &DocumentId,
        version: i64,
        text: String,
    ) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let current_version = current.version.unwrap_or(version);
        if version <= current_version {
            return Err(DocumentError::StaleVersion {
                document: id.clone(),
                current: current_version,
                received: version,
            });
        }
        let document = staged_overlay_document(id.clone(), version, text, current.path.clone());
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Commits a worker-prepared document only while its exact staged text/version is current.
    pub fn commit_prepared_document(&mut self, prepared: PreparedDocument) -> bool {
        let id = prepared.document.id.clone();
        let Some(current) = self.documents.get(&id) else {
            return false;
        };
        let matches_current = current.source == DocumentSource::Overlay
            && current.version == prepared.document.version
            && current.text == prepared.document.text
            && current.path == prepared.document.path;
        if !matches_current {
            return false;
        }
        Arc::make_mut(&mut self.documents).insert(id, prepared.document);
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Applies all changes from one `didChange` notification atomically.
    pub fn apply_document_changes(
        &mut self,
        id: &DocumentId,
        version: i64,
        changes: &[TextChange],
    ) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let current_version = current.version.unwrap_or(version);
        if version <= current_version {
            return Err(DocumentError::StaleVersion {
                document: id.clone(),
                current: current_version,
                received: version,
            });
        }

        let mut text = current.text().to_owned();
        for change in changes {
            if let Some(range) = change.range {
                let start = usize::try_from(range.start()).ok();
                let end = usize::try_from(range.end()).ok();
                let valid = start
                    .zip(end)
                    .is_some_and(|(start, end)| start <= end && text.get(start..end).is_some());
                if !valid {
                    return Err(DocumentError::InvalidRange {
                        document: id.clone(),
                        range,
                    });
                }
                if let (Some(start), Some(end)) = (start, end) {
                    text.replace_range(start..end, &change.text);
                }
            } else {
                text = change.text.clone();
            }
        }

        let path = current.path.clone();
        let document = self.document_snapshot(
            id.clone(),
            Some(version),
            text,
            DocumentSource::Overlay,
            path,
        );
        Arc::make_mut(&mut self.documents).insert(id.clone(), document);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Closes an overlay and restores its current disk candidate when available.
    pub fn close_document(&mut self, id: &DocumentId) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let path = current.path.clone();
        Arc::make_mut(&mut self.documents).remove(id);
        if let Some(path) = path {
            let mut report = WorkspaceScanReport::default();
            if let Some(text) = read_source_file(
                &path,
                WorkspaceScanLimits::default(),
                &mut report,
                self.profile.source_encoding,
            ) {
                let document = self.document_snapshot(
                    id.clone(),
                    None,
                    text,
                    DocumentSource::Disk,
                    Some(path),
                );
                Arc::make_mut(&mut self.documents).insert(id.clone(), document);
            }
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Captures an immutable query view.
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        AnalysisSnapshot {
            revision: self.revision,
            rules: Arc::clone(&self.rules),
            profile: Arc::clone(&self.profile),
            roots: Arc::clone(&self.roots),
            workspace_root: self.workspace_root.clone(),
            documents: Arc::clone(&self.documents),
            source_files: Arc::clone(&self.source_files),
            source_file_paths: Arc::clone(&self.source_file_paths),
            file_states: Arc::clone(&self.file_states),
            index: Arc::clone(&self.index),
            scan_report: Arc::clone(&self.scan_report),
            vanilla_localisation_previews: Arc::clone(&self.vanilla_localisation_previews),
            query_cache: Arc::clone(&self.query_cache),
        }
    }
}

/// Builds the physical-path lookup used to resolve one scanned file without scanning the full
/// file table. First match wins, mirroring the iteration order of the previous linear scans.
fn source_file_paths(files: &BTreeMap<SourceFileId, SourceFile>) -> HashMap<PathBuf, SourceFileId> {
    let mut paths = HashMap::with_capacity(files.len());
    for (id, file) in files {
        paths.entry(file.physical_path.clone()).or_insert(*id);
    }
    paths
}

/// Compares two configured/cached root paths tolerating an offline source directory.
///
/// A cached root keeps the canonical path recorded at build time; the configured root may be a
/// plain absolute path, and its source directory may no longer exist. Equal canonical forms
/// (when both resolve) or identical raw paths are accepted.
fn paths_match(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
