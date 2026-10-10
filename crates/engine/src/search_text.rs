//! Bounded full-text source materialization, independent of semantic file classification.
use crate::AnalysisSnapshot;
use std::sync::Arc;
use text::{AbsPath, LogicalPath};
use vfs::{SourceRootId, WorkspaceError, WorkspaceScanFilters, WorkspaceScanReport};

pub const SEARCH_TEXT_BYTES: usize = 128 * 1024 * 1024;
pub const SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SearchTextFile {
    pub root: SourceRootId,
    pub path: LogicalPath,
    pub physical_path: AbsPath,
    pub text: Arc<str>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchTexts {
    pub files: Vec<SearchTextFile>,
    pub generation: u64,
    pub limitations: Vec<String>,
    omitted_limitations: usize,
}

impl SearchTexts {
    fn note(&mut self, message: String) {
        if self.limitations.len() < 100 {
            self.limitations.push(message);
        } else {
            self.omitted_limitations += 1;
        }
    }
}

impl AnalysisSnapshot {
    /// Source-control and ParadoxCode cache metadata are outside the text-source corpus.
    #[must_use]
    pub fn text_search_path_allowed(&self, path: &LogicalPath) -> bool {
        !source_metadata(path) && !self.scan_filters.ignores_file(path.as_str())
    }
    /// Materialize disk text once per index state and watch epoch, so pages cannot reread changed disk
    /// bytes into an older query. Open-document ownership is applied by the IDE query layer.
    pub fn search_texts(
        &self,
        selected_roots: Option<&[u32]>,
        epoch: u64,
        include: &WorkspaceScanFilters,
        exclude: &WorkspaceScanFilters,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Arc<SearchTexts>, WorkspaceError> {
        let mut roots_key = selected_roots.map(<[u32]>::to_vec);
        if let Some(roots) = &mut roots_key {
            roots.sort_unstable();
            roots.dedup();
        }
        let key = format!(
            "editor-search-texts:{epoch}:{roots_key:?}:{:?}:{:?}",
            include.ignore_file_patterns(),
            exclude.ignore_file_patterns()
        );
        if cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        if let Some(cached) = self.query_cache().get(self.revision(), &key) {
            return Ok(cached);
        }
        let mut result = SearchTexts::default();
        let mut count = 0usize;
        let mut bytes = 0usize;
        let mut limits = self.scan_limits();
        limits.max_file_size = limits.max_file_size.min(SEARCH_FILE_BYTES);
        let mut report = WorkspaceScanReport::default();
        let excluded_directories = WorkspaceScanFilters::new(
            Vec::new(),
            exclude
                .ignore_file_patterns()
                .iter()
                .filter_map(|pattern| pattern.strip_suffix("/**").map(str::to_owned))
                .collect(),
        )
        .unwrap_or_default();
        'roots: for root in self.source_roots() {
            if selected_roots.is_some_and(|roots| !roots.contains(&root.id.get())) {
                continue;
            }
            let mut stack = vec![(root.path.clone(), 0usize)];
            while let Some((directory, depth)) = stack.pop() {
                if cancelled() {
                    return Err(WorkspaceError::Cancelled);
                }
                let entries = match std::fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        result.note(format!("{}: {error}", directory.display()));
                        continue;
                    }
                };
                let mut available = Vec::new();
                for entry in entries.take(limits.max_files.saturating_add(1)) {
                    if cancelled() {
                        return Err(WorkspaceError::Cancelled);
                    }
                    match entry {
                        Ok(entry) => available.push(entry),
                        Err(error) => result.note(format!("{}: {error}", directory.display())),
                    }
                }
                if available.len() > limits.max_files {
                    result.note(format!(
                        "{}: directory entry budget reached",
                        directory.display()
                    ));
                }
                let mut entries = available;
                entries.sort_by_key(std::fs::DirEntry::file_name);
                for entry in entries {
                    if cancelled() {
                        return Err(WorkspaceError::Cancelled);
                    }
                    let absolute = AbsPath::normalize(&entry.path());
                    let Ok(relative) = absolute.strip_prefix(&root.path) else {
                        continue;
                    };
                    let Ok(logical) = LogicalPath::parse(&relative.to_string_lossy()) else {
                        continue;
                    };
                    if source_metadata(&logical) {
                        continue;
                    }
                    let metadata = match entry.file_type() {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            result.note(format!("{}: {error}", absolute.display()));
                            continue;
                        }
                    };
                    if metadata.is_dir() {
                        if self.scan_filters.ignores_directory(logical.as_str())
                            || excluded_directories.ignores_directory(logical.as_str())
                            || !directory_can_match(
                                include.ignore_file_patterns(),
                                logical.as_str(),
                            )
                        {
                            continue;
                        }
                        count += 1;
                        if count > limits.max_files {
                            result.note(format!(
                                "text discovery stopped at {} entries",
                                limits.max_files
                            ));
                            break 'roots;
                        }
                        if depth >= limits.max_depth {
                            result.note(format!(
                                "{}: directory depth budget reached",
                                absolute.display()
                            ));
                        } else {
                            stack.push((absolute, depth + 1));
                        }
                        continue;
                    }
                    if self.scan_filters.ignores_file(logical.as_str())
                        || exclude.ignores_file(logical.as_str())
                        || (!include.ignore_file_patterns().is_empty()
                            && !include.ignores_file(logical.as_str()))
                    {
                        continue;
                    }
                    count += 1;
                    if count > limits.max_files {
                        result.note(format!(
                            "text discovery stopped at {} entries",
                            limits.max_files
                        ));
                        break 'roots;
                    }
                    if metadata.is_symlink() {
                        result.note(format!(
                            "{}: symbolic links are not followed",
                            absolute.display()
                        ));
                        continue;
                    }
                    if !metadata.is_file() {
                        continue;
                    }
                    // These game assets are known binary categories, not unknown text failures.
                    let extension = absolute
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if [
                        "dds", "tga", "png", "jpg", "jpeg", "bmp", "webp", "ogg", "wav", "mp3",
                        "ttf", "otf", "zip", "bin", "exe", "dll", "pdcindex", "sqlite", "db",
                    ]
                    .contains(&extension.as_str())
                    {
                        continue;
                    }
                    let text = if let Some(text) = self
                        .source_file_id_for_path(&absolute)
                        .and_then(|id| self.source_text(id))
                    {
                        if text.len() as u64 > limits.max_file_size {
                            result.note(format!(
                                "{}: exceeds {} byte text-file budget",
                                absolute.display(),
                                limits.max_file_size
                            ));
                            continue;
                        }
                        text.to_owned()
                    } else {
                        let Some(text) = vfs::scan::read_source_file(
                            &absolute,
                            limits,
                            &mut report,
                            self.game_profile().source_encoding,
                        ) else {
                            continue;
                        };
                        text
                    };
                    if text.contains('\0') {
                        result.note(format!(
                            "{}: binary or unsupported text encoding",
                            absolute.display()
                        ));
                        continue;
                    }
                    bytes = bytes.saturating_add(text.len());
                    if bytes > SEARCH_TEXT_BYTES {
                        result.note(format!("text corpus exceeds {SEARCH_TEXT_BYTES} byte memory budget; narrow the configured roots"));
                        break 'roots;
                    }
                    result.files.push(SearchTextFile {
                        root: root.id,
                        path: logical,
                        physical_path: absolute,
                        text: Arc::from(text),
                    });
                }
            }
        }
        for issue in &report.issues {
            result.note(format!("{}: {}", issue.path.display(), issue.detail));
        }
        if report.omitted_issues > 0 {
            result.note(format!(
                "{} additional read issues omitted",
                report.omitted_issues
            ));
        }
        result
            .files
            .sort_by_key(|file| (file.root, file.path.clone()));
        if result.omitted_limitations > 0 {
            result.limitations.push(format!(
                "{} additional coverage limitations omitted",
                result.omitted_limitations
            ));
        }
        if cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        result.generation = NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = Arc::new(result);
        self.query_cache().insert(
            self.revision(),
            crate::CacheDomain::SearchTexts,
            key,
            Arc::clone(&result),
        );
        Ok(result)
    }
}

fn source_metadata(path: &LogicalPath) -> bool {
    path.as_str()
        .split('/')
        .any(|part| matches!(part, ".git" | ".hg" | ".svn" | ".pdc"))
}

/// Only a literal leading path can safely prune a subtree. Basename and leading-wildcard
/// patterns retain all directories and let the VFS matcher decide individual files.
fn directory_can_match(patterns: &[String], directory: &str) -> bool {
    patterns.is_empty()
        || patterns.iter().any(|pattern| {
            if !pattern.contains('/') {
                return true;
            }
            let prefix = pattern
                .split('/')
                .take_while(|part| !part.contains(['*', '?']))
                .collect::<Vec<_>>()
                .join("/");
            prefix.is_empty()
                || prefix == directory
                || prefix
                    .strip_prefix(directory)
                    .is_some_and(|rest| rest.starts_with('/'))
                || directory
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}
