//! Mutable workspace owner and atomic state transitions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pdx_rules::{GameProfile, ParserKind, RuleSet};
use pdx_text::LogicalPath;

use crate::index::{FileIndexShard, LocalisationPreviewMap, PositionMap, WorkspaceIndex};
use crate::index_cache::{IndexCache, IndexCacheError};
use crate::model::{
    DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId, DocumentSnapshot,
    DocumentSource, FileState, PreparedDocument, SourceFile, SourceFileId, SourceRoot,
    SourceRootId, SourceRootKind, TextChange, WorkspaceChange, WorkspaceError,
    WorkspaceScanFilters, WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanReport,
    WorkspaceScanToken,
};
use crate::parse_cache::ParseCache;
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
    installed_caches: BTreeSet<SourceRootId>,
    localisation_previews: Arc<LocalisationPreviewMap>,
    query_cache: Arc<SnapshotQueryCache>,
    parse_cache: Option<ParseCache>,
    scan_filters: Arc<WorkspaceScanFilters>,
    scan_limits: WorkspaceScanLimits,
    preferred_localisation_languages: Arc<[String]>,
    completion_source_layers: Arc<[SourceRootKind]>,
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
            installed_caches: BTreeSet::new(),
            localisation_previews: Arc::new(LocalisationPreviewMap::new()),
            query_cache: Arc::new(SnapshotQueryCache::new()),
            parse_cache: None,
            scan_filters: Arc::new(WorkspaceScanFilters::default()),
            scan_limits: WorkspaceScanLimits::default(),
            preferred_localisation_languages: Arc::from([]),
            completion_source_layers: Arc::from([
                SourceRootKind::CurrentMod,
                SourceRootKind::Dependency,
                SourceRootKind::Vanilla,
            ]),
        }
    }

    /// Returns a clone of this host that writes and reads syntax trees below `directory`.
    ///
    /// The cache stores only parser output. HIR and rule-dependent semantic state are always
    /// rebuilt for the active snapshot, so changing a rule artifact cannot reuse stale semantics.
    #[must_use]
    pub fn with_parse_cache_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.parse_cache = Some(ParseCache::new(directory));
        self
    }

    /// Enables or disables the persistent syntax-tree cache for subsequent disk scans.
    pub fn set_parse_cache_dir(&mut self, directory: Option<PathBuf>) {
        self.parse_cache = directory.map(ParseCache::new);
    }

    /// Returns the configured syntax-tree cache directory, if one is active.
    #[must_use]
    pub fn parse_cache_dir(&self) -> Option<&Path> {
        self.parse_cache.as_ref().map(ParseCache::directory)
    }

    /// Replaces the bounded file/directory filters used by subsequent workspace scans.
    ///
    /// The host does not scan immediately; callers can apply the filters and then choose when
    /// to refresh, preserving the same atomic refresh boundary as other workspace configuration.
    pub fn set_scan_filters(&mut self, filters: WorkspaceScanFilters) {
        if self.scan_filters.as_ref() != &filters {
            self.scan_filters = Arc::new(filters);
            self.advance_revision();
        }
    }

    /// Returns the active file/directory scan filters.
    #[must_use]
    pub fn scan_filters(&self) -> &WorkspaceScanFilters {
        self.scan_filters.as_ref()
    }

    /// Replaces the bounded resource profile used by subsequent workspace scans.
    pub fn set_scan_limits(&mut self, limits: WorkspaceScanLimits) {
        if self.scan_limits != limits {
            self.scan_limits = limits;
            self.advance_revision();
        }
    }

    /// Returns the active bounded scan profile.
    #[must_use]
    pub const fn scan_limits(&self) -> WorkspaceScanLimits {
        self.scan_limits
    }

    /// Replaces the preferred localisation language order used by analysis queries.
    ///
    /// This order also decides which localisation previews are retained when an index cache
    /// is installed (together with the English fallback), so set preferences before
    /// installing caches — exactly how the LSP applies them at initialize. Definitions for
    /// every language stay indexed regardless of this order.
    pub fn set_preferred_localisation_languages(&mut self, languages: Vec<String>) {
        let languages = languages
            .into_iter()
            .map(|language| language.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if self.preferred_localisation_languages.as_ref() != languages.as_slice() {
            self.preferred_localisation_languages = Arc::from(languages);
            self.advance_revision();
        }
    }

    /// Replaces the source layers eligible to provide completion members.
    pub fn set_completion_source_layers(&mut self, layers: Vec<SourceRootKind>) {
        if self.completion_source_layers.as_ref() != layers.as_slice() {
            self.completion_source_layers = Arc::from(layers);
            self.advance_revision();
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
                self.installed_caches = BTreeSet::new();
                self.localisation_previews = Arc::new(LocalisationPreviewMap::new());
            }
            WorkspaceChange::SetWorkspaceRoot(root) => self.workspace_root = root,
        }
        self.advance_revision();
    }

    fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.query_cache.advance_to(self.revision);
    }

    /// Advances the revision for a change that only affects overlay documents.
    ///
    /// Document opens, edits, and closes leave the workspace index untouched,
    /// so index-derived cache entries stay valid across keystrokes instead of
    /// being rebuilt per edit.
    fn advance_document_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.query_cache.advance_documents(self.revision);
    }

    /// Installs a validated persistent index cache for any configured source root.
    ///
    /// The cached root may already be configured (a dependency with an explicit index): its
    /// identity must then match the configured root and the cache replaces live scanning for it.
    /// An unknown cached root (the Vanilla installation) is inserted at the front of the root
    /// order. The cache's rule hash is intentionally not required to match; it remains observable
    /// in metadata so the caller can decide whether to refresh.
    pub fn install_index_cache(&mut self, cache: IndexCache) -> Result<(), IndexCacheError> {
        self.install_index_caches([cache])
    }

    /// Installs several validated persistent index caches and rebuilds the combined workspace
    /// index only once.
    ///
    /// Cache validation and source-file collision checks happen before mutating the host. The
    /// input order is retained for root validation and the final source-priority calculation, so
    /// batching does not change the configured overlay semantics.
    pub fn install_index_caches(
        &mut self,
        caches: impl IntoIterator<Item = IndexCache>,
    ) -> Result<(), IndexCacheError> {
        let caches = caches.into_iter().collect::<Vec<_>>();
        if caches.is_empty() {
            return Ok(());
        }

        let mut roots = self.roots.to_vec();
        let mut files = BTreeMap::new();
        let mut shards = Vec::new();
        let mut cached_positions = PositionMap::new();
        let mut cached_previews = LocalisationPreviewMap::new();
        let mut cached_root_ids = BTreeSet::new();

        for cache in caches {
            if cache.metadata().game_id != self.rules.game_id()
                || cache.metadata().game_id != self.profile.game_id
            {
                return Err(IndexCacheError::GameMismatch {
                    expected: self.profile.game_id.clone(),
                    actual: cache.metadata().game_id.clone(),
                });
            }
            let (_, cached_root, cache_files, cached_index, cache_positions, mut cache_previews) =
                cache.into_parts();
            match roots.iter().find(|root| root.id == cached_root.id) {
                Some(configured) => {
                    if configured.kind != cached_root.kind {
                        return Err(IndexCacheError::InvalidData(format!(
                            "cached root kind {:?} does not match configured root {} for id {}",
                            cached_root.kind,
                            configured.path.display(),
                            cached_root.id.get()
                        )));
                    }
                    if !paths_match(&configured.path, &cached_root.path) {
                        return Err(IndexCacheError::InvalidData(format!(
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
                        if root.kind == cached_root.kind
                            && cached_root.kind != SourceRootKind::Dependency
                        {
                            return Err(IndexCacheError::InvalidData(format!(
                                "a {:?} source root is already configured",
                                cached_root.kind
                            )));
                        }
                        if cached_root.path.starts_with(&root.path)
                            || root.path.starts_with(&cached_root.path)
                        {
                            return Err(IndexCacheError::RootConflict {
                                root: cached_root.path.clone(),
                                configured: root.path.clone(),
                            });
                        }
                    }
                    if cached_root.kind == SourceRootKind::Vanilla {
                        roots.insert(0, cached_root.clone());
                    } else {
                        let insert_at = roots
                            .iter()
                            .position(|root| root.order > cached_root.order)
                            .unwrap_or(roots.len());
                        roots.insert(insert_at, cached_root.clone());
                    }
                }
            }
            if !cached_root_ids.insert(cached_root.id) {
                return Err(IndexCacheError::InvalidData(format!(
                    "duplicate cached source root id {}",
                    cached_root.id.get()
                )));
            }

            if let Some(file) = cache_files
                .values()
                .find(|file| !self.profile.allows_scan_file(file.logical_path.as_str()))
            {
                return Err(IndexCacheError::InvalidData(format!(
                    "cache file {} is outside the active profile scan whitelist",
                    file.logical_path.as_str()
                )));
            }
            // Preview retention runs before `cache_files` is moved into `files` so the filter
            // can resolve each preview's file path.
            Self::retain_preferred_localisation_previews(
                &mut cache_previews,
                &cache_files,
                &self.preferred_localisation_languages,
            );
            for (id, file) in cache_files {
                let current_path = file.physical_path.clone();
                if let Some(previous) = files.insert(id, file) {
                    return Err(IndexCacheError::InvalidData(format!(
                        "file id collision between {} and {}",
                        previous.physical_path.display(),
                        current_path.display()
                    )));
                }
            }
            shards.extend(cached_index.shards.into_values());
            cached_positions.merge(cache_positions);
            cached_previews.merge(cache_previews);
        }

        for (id, file) in self.source_files.iter() {
            if let Some(cached) = files.insert(*id, file.clone()) {
                return Err(IndexCacheError::InvalidData(format!(
                    "file id collision between {} and {}",
                    cached.physical_path.display(),
                    file.physical_path.display()
                )));
            }
        }
        shards.extend(self.index.shards.values().cloned());
        // One combined build sets the case policy and derives the lookup maps together, so the
        // merged cache + workspace shards are not rebuilt twice.
        let mut index = WorkspaceIndex::from_shards_with_rules(shards, self.rules.as_ref());
        // Source-file IDs were checked for collisions above, so the cached and existing position
        // keys are disjoint. Merge the existing snapshot positions in one pass: calling
        // `replace_position_ranges` once per Current Mod file would repeatedly rebuild the
        // complete (often-million-entry) position map and turn cache installation quadratic.
        let cached_position_count = cached_positions.len();
        let existing_positions = self.index.position_ranges();
        let existing_position_count = existing_positions.len();
        let mut position_ranges = cached_positions;
        position_ranges.extend(
            existing_positions
                .iter()
                .map(|(key, position)| (key, *position)),
        );
        debug_assert_eq!(
            position_ranges.len(),
            cached_position_count.saturating_add(existing_position_count),
            "source-file collision validation should make position keys disjoint"
        );
        index.replace_all_position_ranges(position_ranges);
        let priorities = source_priorities(&roots, &files);
        index.resolve_priorities(&priorities, self.rules.as_ref());

        let mut localisation_previews = (*self.localisation_previews).clone();
        localisation_previews.merge(cached_previews);
        self.roots = Arc::from(roots);
        self.source_files = Arc::new(files);
        self.source_file_paths = Arc::new(source_file_paths(&self.source_files));
        self.index = Arc::new(index);
        self.localisation_previews = Arc::new(localisation_previews);
        self.installed_caches.extend(cached_root_ids);
        self.advance_revision();
        Ok(())
    }

    /// Bounds resident memory for cache-installed localisation previews to the languages
    /// analysis can surface: the configured preference order plus the English fallback that
    /// `prefer_localisation_language_for_snapshot` selects. Cached Vanilla locates every key
    /// once per language, so unpreferred languages hold the bulk of preview bytes while no
    /// query can reach them. Dropped previews stay in the persistent `.pdxindex` and return
    /// on the next session that prefers them; diagnostics never read previews. Files without
    /// a path language marker are always retained, as are files unknown to this cache's
    /// source table (load-time validation guarantees the latter cannot occur).
    fn retain_preferred_localisation_previews(
        previews: &mut LocalisationPreviewMap,
        files: &BTreeMap<SourceFileId, SourceFile>,
        preferred: &[String],
    ) {
        previews.retain_files(|file_id| {
            files.get(&file_id).is_none_or(|file| {
                Self::localisation_path_language(file.logical_path.as_str()).is_none_or(
                    |language| {
                        language.eq_ignore_ascii_case("english")
                            || preferred
                                .iter()
                                .any(|preferred| preferred.eq_ignore_ascii_case(language))
                    },
                )
            })
        });
    }

    /// Extracts the localisation language from a logical path. `localisation/l_english/…`
    /// directories and `localisation/name_l_english.yml` file stems both yield `english`;
    /// paths without a language marker yield `None`. Mirrors `pdx-analysis`'s
    /// `localisation_language` so retention covers exactly the candidates analysis selects.
    fn localisation_path_language(path: &str) -> Option<&str> {
        for segment in path.split('/') {
            if let Some(language) = segment
                .strip_prefix("l_")
                .filter(|language| !language.is_empty())
            {
                let language = language
                    .strip_suffix(".yml")
                    .or_else(|| language.strip_suffix(".yaml"))
                    .unwrap_or(language);
                return (!language.is_empty()).then_some(language);
            }
        }
        let file = path.rsplit('/').next()?;
        let after = file
            .rfind("_l_")
            .map(|index| &file[index + 3..])
            .unwrap_or_default();
        let language = after
            .strip_suffix(".yml")
            .or_else(|| after.strip_suffix(".yaml"))
            .unwrap_or(after);
        (!language.is_empty()).then_some(language)
    }

    /// Scans all configured roots in stable order and atomically refreshes source files and shards.
    pub fn refresh_source_roots(&mut self) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits(self.scan_limits)
    }

    /// Scans configured roots while cooperatively observing `cancellation`.
    pub fn refresh_source_roots_cancellable(
        &mut self,
        cancellation: &WorkspaceScanToken,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(self.scan_limits, cancellation, None)
    }

    /// Scans configured roots while cooperatively observing `cancellation`, invoking `progress`
    /// with `(completed, total)` source files so long-running background work can be surfaced.
    pub fn refresh_source_roots_cancellable_with_progress(
        &mut self,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<WorkspaceScanReport, WorkspaceError> {
        self.refresh_source_roots_with_limits_and_cancellation(
            self.scan_limits,
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
                self.scan_filters.as_ref(),
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
                    // Closed files never retain scan frontends: shards and
                    // position ranges are extracted during the scan, and the
                    // diagnostics pass reparses/lowerers transiently per file.
                    // This keeps the peak resident set bounded by steady index
                    // structures instead of every file's CST+HIR at once.
                    retain_frontend: false,
                });
            }
        }
        let source_context = SourceLoadContext {
            limits,
            previous_files: self.source_files.as_ref(),
            previous_states: self.file_states.as_ref(),
            rules: self.rules.as_ref(),
            profile: self.profile.as_ref(),
            parse_cache: self.parse_cache.as_ref(),
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
            .map(|state| state.shard_handle())
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
        position_ranges.retain_files(|file_id| {
            files.contains_key(&file_id) && !file_states.contains_key(&file_id)
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
        self.advance_revision();
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
        let limits = self.scan_limits;
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
            let ignored = self.scan_filters.ignores_file(logical.as_str());
            if ignored && change.kind != DiskFileChangeKind::Deleted {
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
                Some(text) => {
                    let state = build_file_state(
                        &source_file,
                        text,
                        file_revision,
                        self.rules.as_ref(),
                        self.profile.as_ref(),
                    );
                    // Same retention policy as the scan: extract positions,
                    // then drop the frontend so closed files never hold a
                    // CST/HIR tree. The index map keeps the only copy.
                    index.replace_position_ranges(id, position_ranges_for_state(&state));
                    state.cache_only()
                }
                None => empty_file_state(&source_file, file_revision),
            });
            files.insert(id, source_file);
            paths.insert(change.path.clone(), id);
            file_states.insert(id, Arc::clone(&state));
            let priorities = source_priorities(&self.roots, &files);
            index.replace_shard_resolved(state.shard_handle(), &priorities, self.rules.as_ref());
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
            self.advance_revision();
        }
        Ok(report)
    }

    /// Returns a mutable workspace index for targeted shard replacement.
    pub fn replace_index_shard(&mut self, shard: FileIndexShard) {
        let file_id = shard.file_id;
        let priorities = source_priorities(&self.roots, &self.source_files);
        let shard = Arc::new(shard);
        Arc::make_mut(&mut self.index).replace_shard_resolved(
            Arc::clone(&shard),
            &priorities,
            self.rules.as_ref(),
        );
        Arc::make_mut(&mut self.index).remove_position_ranges(file_id);
        if let Some(previous) = self.file_states.get(&file_id) {
            let mut replacement = previous.as_ref().clone();
            replacement.shard = Arc::clone(&shard);
            Arc::make_mut(&mut self.index)
                .replace_position_ranges(file_id, position_ranges_for_state(&replacement));
            Arc::make_mut(&mut self.file_states).insert(file_id, Arc::new(replacement));
        }
        self.advance_revision();
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
        self.advance_document_revision();
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
        self.advance_revision();
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
        self.advance_document_revision();
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
        self.advance_revision();
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
        self.advance_document_revision();
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
                self.scan_limits,
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
        self.advance_document_revision();
        Ok(())
    }

    /// Captures an immutable query view.
    #[must_use]
    /// Evicts the retained CST/HIR frontends of source files, keeping source
    /// text, index shards, and cached positions.
    ///
    /// `keep` guards files that must stay warm (for example files backing open
    /// editor overlays are fine to evict, so callers usually keep nothing).
    /// The workspace revision is unchanged: shards and index answers are
    /// identical, so snapshot query caches remain valid. Returns the number of
    /// files whose frontends were dropped.
    pub fn evict_source_frontends(&mut self, keep: &dyn Fn(SourceFileId) -> bool) -> usize {
        let mut evicted = Vec::new();
        for (id, state) in self.file_states.iter() {
            if keep(*id) {
                continue;
            }
            if let Some(evicted_state) = state.evict_frontend() {
                evicted.push((*id, Arc::new(evicted_state)));
            }
        }
        if evicted.is_empty() {
            return 0;
        }
        let evicted_count = evicted.len();
        let mut file_states = BTreeMap::clone(&self.file_states);
        for (id, state) in evicted {
            file_states.insert(id, state);
        }
        self.file_states = Arc::new(file_states);
        evicted_count
    }

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
            localisation_previews: Arc::clone(&self.localisation_previews),
            query_cache: Arc::clone(&self.query_cache),
            scan_limits: self.scan_limits,
            preferred_localisation_languages: Arc::clone(&self.preferred_localisation_languages),
            completion_source_layers: Arc::clone(&self.completion_source_layers),
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
