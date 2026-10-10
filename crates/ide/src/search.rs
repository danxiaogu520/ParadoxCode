//! Snapshot-bound editor search. External-client query contracts remain separate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use engine::{AnalysisSnapshot, DocumentSource, SourceRootId};
use parser::{CstKind, FileFormat};

use crate::resolution::twin_suppressed_overlays;
use crate::support::{
    ParsedContent, ParsedInput, input_for_document, syntax_input_for_source_file,
};
use crate::{CancellationToken, Cancelled, Location};

const LOCALISATION_BYTES: usize = 256 * 1024 * 1024;
const LOCALISATION_REUSE_KEY: &str = "editor-localisation-file-reuse";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LocalisationInput {
    Disk(vfs::SourceFileId),
    Overlay(vfs::DocumentId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalisationDiskStamp {
    bytes: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl LocalisationDiskStamp {
    fn read(path: &std::path::Path) -> Option<Self> {
        let archive = vfs::scan::split_archive_path(path);
        let path = archive.as_ref().map_or(path, |(path, _)| path.as_path());
        std::fs::metadata(path)
            .ok()
            .map(|metadata| Self::from_metadata(&metadata))
    }
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

#[derive(Debug)]
struct LocalisationValue {
    key_range: text::TextRange,
    value_range: text::TextRange,
    readable_range: text::TextRange,
    folded_key: Option<Box<str>>,
    readable: Option<String>,
    language: Arc<str>,
    full_value_available: bool,
}
#[derive(Debug)]
struct LocalisationFile {
    source: Arc<str>,
    descriptor: Option<vfs::SourceFile>,
    disk_backed: bool,
    disk_stamp: Option<LocalisationDiskStamp>,
    document: Option<vfs::DocumentId>,
    physical: Option<text::AbsPath>,
    file: Option<vfs::SourceFileId>,
    path: Option<text::LogicalPath>,
    root: Option<SourceRootId>,
    version: Option<i64>,
    values: Vec<LocalisationValue>,
    limitations: Vec<String>,
    retained_bytes: usize,
    budget_reached: bool,
}

/// One version shares its file's source and parsed values, without copying plain strings.
#[derive(Clone, Debug)]
pub struct EditorLocalisationEntry {
    data: Arc<LocalisationFile>,
    index: usize,
    pub active: bool,
}
impl EditorLocalisationEntry {
    fn record(&self) -> &LocalisationValue {
        &self.data.values[self.index]
    }
    pub fn key(&self) -> &str {
        self.data.slice(self.record().key_range)
    }
    pub fn normalised_key(&self) -> &str {
        self.record()
            .folded_key
            .as_deref()
            .unwrap_or_else(|| self.key())
    }
    pub fn language(&self) -> &str {
        &self.record().language
    }
    pub fn value(&self) -> &str {
        self.record()
            .readable
            .as_deref()
            .unwrap_or_else(|| self.data.slice(self.record().readable_range))
    }
    pub fn full_value_available(&self) -> bool {
        self.record().full_value_available
    }
    pub fn source(&self) -> &Arc<str> {
        &self.data.source
    }
    pub fn root(&self) -> Option<SourceRootId> {
        self.data.root
    }
    pub fn version(&self) -> Option<i64> {
        self.data.version
    }
    pub fn document(&self) -> Option<&vfs::DocumentId> {
        self.data.document.as_ref()
    }
    pub fn file(&self) -> Option<vfs::SourceFileId> {
        self.data.file
    }
    pub fn path(&self) -> Option<&text::LogicalPath> {
        self.data.path.as_ref()
    }
    pub fn key_range(&self) -> text::TextRange {
        self.record().key_range
    }
    pub fn value_range(&self) -> text::TextRange {
        self.record().value_range
    }
    pub fn location(&self) -> Location {
        Location {
            document: self.data.document.clone(),
            file: self.data.file,
            path: self.data.path.clone(),
            range: self.key_range(),
        }
    }
}
impl LocalisationFile {
    fn slice(&self, range: text::TextRange) -> &str {
        self.source
            .get(range.start() as usize..range.end() as usize)
            .unwrap_or("")
    }
}

#[derive(Debug)]
struct LocalisationMemo {
    corpus: Arc<EditorLocalisations>,
    catalog: Arc<BTreeMap<vfs::SourceFileId, vfs::SourceFile>>,
    epoch: u64,
}

/// Complete values retain transformed text only; plain keys and values borrow their file source.
#[derive(Debug, Default)]
pub struct EditorLocalisations {
    pub entries: Vec<EditorLocalisationEntry>,
    pub generation: u64,
    pub limitations: Vec<String>,
    files: BTreeMap<LocalisationInput, Arc<LocalisationFile>>,
    roots: Vec<engine::SourceRoot>,
    policy: String,
    scan_notes: Vec<String>,
    effective: Vec<bool>,
    languages: Vec<String>,
    retained_bytes: usize,
    budget_reached: bool,
}
pub fn editor_localisations(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<EditorLocalisations>, Cancelled> {
    editor_localisations_with_epoch(snapshot, 0, cancellation)
}

/// Validates file inputs before sharing unchanged data across document revisions and watch epochs.
pub fn editor_localisations_with_epoch(
    snapshot: &AnalysisSnapshot,
    epoch: u64,
    cancellation: &CancellationToken,
) -> Result<Arc<EditorLocalisations>, Cancelled> {
    cancellation.checkpoint()?;
    let cache_key = format!("editor-localisations:{}:{epoch}", snapshot.revision());
    if let Some(data) = snapshot.query_cache().get(snapshot.revision(), &cache_key) {
        return Ok(data);
    }
    let policy = format!(
        "{}:{:?}:{:?}",
        snapshot.ir_fingerprint(),
        snapshot.scan_limits(),
        snapshot.game_profile().source_encoding
    );
    let previous = snapshot
        .query_cache()
        .get::<LocalisationMemo>(snapshot.revision(), LOCALISATION_REUSE_KEY)
        .filter(|memo| memo.corpus.policy == policy);
    let catalog = snapshot.source_files_handle();
    let same_catalog = previous
        .as_ref()
        .is_some_and(|old| Arc::ptr_eq(&old.catalog, &catalog));
    let same_epoch = previous.as_ref().is_some_and(|old| old.epoch == epoch);
    let same_roots = previous
        .as_ref()
        .is_some_and(|old| old.corpus.roots == snapshot.source_roots());
    let mut data = EditorLocalisations {
        policy,
        roots: snapshot.source_roots().to_vec(),
        ..Default::default()
    };
    let hidden = crate::support::overlay_file_ids(snapshot);
    let candidates = snapshot
        .source_files()
        .values()
        .filter(|file| {
            !hidden.contains(&file.id)
                && snapshot
                    .rules()
                    .classify(&file.logical_path)
                    .is_some_and(|category| category.parser == rules::ParserKind::Localisation)
        })
        .collect::<Vec<_>>();
    let workers = if previous.is_none() {
        snapshot.scan_limits().max_workers.clamp(1, 2)
    } else {
        1
    };
    for batch in candidates.chunks(workers) {
        cancellation.checkpoint()?;
        if data.budget_reached {
            break;
        }
        let remaining = LOCALISATION_BYTES.saturating_sub(data.retained_bytes);
        let prefetched = if workers > 1 && batch.len() > 1 && remaining >= 1024 * 1024 {
            let allowance = remaining / batch.len();
            let parts = std::thread::scope(|scope| {
                let jobs = batch
                    .iter()
                    .map(|file| {
                        scope.spawn(move || {
                            load_localisation_file(snapshot, file, allowance, cancellation)
                        })
                    })
                    .collect::<Vec<_>>();
                jobs.into_iter()
                    .map(|job| {
                        job.join()
                            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            // A large file uses the full remaining allowance in a serial retry, never a smaller worker quota.
            if parts
                .iter()
                .any(|part| part.budget_reached || part.retained_bytes > allowance)
            {
                None
            } else {
                Some(parts)
            }
        } else {
            None
        };
        for (file_index, file) in batch.iter().enumerate() {
            cancellation.checkpoint()?;
            if data.budget_reached {
                break;
            }
            if let Some(parts) = &prefetched {
                retain_localisation_file(
                    &mut data,
                    LocalisationInput::Disk(file.id),
                    Arc::clone(&parts[file_index]),
                );
                continue;
            }
            let key = LocalisationInput::Disk(file.id);
            let old = previous.as_ref().and_then(|old| old.corpus.files.get(&key));
            let reusable = old.filter(|old| {
                if old.budget_reached || old.descriptor.as_ref() != Some(file) {
                    return false;
                }
                if let Some(state) = snapshot.file_state(file.id) {
                    return !old.disk_backed && Arc::ptr_eq(&state.source_handle(), &old.source);
                }
                if !old.disk_backed {
                    return false;
                }
                if same_catalog && same_epoch {
                    return true;
                }
                let stamp = LocalisationDiskStamp::read(&file.physical_path);
                if stamp != old.disk_stamp || (stamp.is_none() && !old.source.is_empty()) {
                    return false;
                }
                // Unix change time/inode detect writes or replacement even if mtime is restored.
                #[cfg(unix)]
                {
                    true
                }
                #[cfg(not(unix))]
                {
                    if same_epoch {
                        return true;
                    }
                    let mut report = vfs::WorkspaceScanReport::default();
                    vfs::scan::read_source_file(
                        &file.physical_path,
                        snapshot.scan_limits(),
                        &mut report,
                        snapshot.game_profile().source_encoding,
                    )
                    .is_some_and(|source| source == old.source.as_ref())
                }
            });
            let part = if let Some(old) = reusable {
                Arc::clone(old)
            } else {
                load_localisation_file(
                    snapshot,
                    file,
                    LOCALISATION_BYTES.saturating_sub(data.retained_bytes),
                    cancellation,
                )?
            };
            retain_localisation_file(&mut data, key, part);
        }
    }
    let suppressed = twin_suppressed_overlays(snapshot);
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if data.budget_reached {
            break;
        }
        if document.source() != DocumentSource::Overlay
            || suppressed.contains(document.id())
            || document
                .parsed()
                .is_none_or(|parsed| parsed.format() != FileFormat::Localisation)
        {
            continue;
        }
        let key = LocalisationInput::Overlay(document.id().clone());
        let old = previous
            .as_ref()
            .and_then(|old| old.corpus.files.get(&key))
            .filter(|old| {
                same_roots
                    && !old.budget_reached
                    && old.version == document.version()
                    && old.physical.as_deref() == document.path()
                    && Arc::ptr_eq(&old.source, &document.text_handle())
            });
        let part = if let Some(old) = old {
            Arc::clone(old)
        } else {
            let Some(input) = input_for_document(snapshot, document.id()) else {
                continue;
            };
            Arc::new(collect_localisations(
                snapshot,
                &input,
                LOCALISATION_BYTES.saturating_sub(data.retained_bytes),
                cancellation,
            )?)
        };
        retain_localisation_file(&mut data, key, part);
    }
    data.scan_notes = snapshot
        .scan_report()
        .issues
        .iter()
        .filter(|issue| issue.kind == vfs::WorkspaceScanIssueKind::EncodingRecovered)
        .map(|issue| format!("{}: {}", issue.path.display(), issue.detail))
        .collect();
    if snapshot.scan_report().skipped_entries > 0 {
        data.scan_notes.push(format!(
            "workspace scan skipped {} entries; some sources may be missing",
            snapshot.scan_report().skipped_entries
        ));
    }
    data.limitations.extend(data.scan_notes.iter().cloned());
    data.effective = data
        .files
        .values()
        .map(|part| snapshot.source_is_effective(part.document.as_ref(), part.file))
        .collect();
    if let Some(previous) = previous.as_ref().filter(|old| {
        same_roots
            && old.corpus.scan_notes == data.scan_notes
            && old.corpus.effective == data.effective
            && old.corpus.files.len() == data.files.len()
            && old.corpus.files.iter().all(|(key, file)| {
                data.files
                    .get(key)
                    .is_some_and(|current| Arc::ptr_eq(file, current))
            })
            && old.corpus.limitations == data.limitations
    }) {
        cancellation.checkpoint()?;
        let result = Arc::clone(&previous.corpus);
        snapshot.query_cache().insert(
            snapshot.revision(),
            engine::CacheDomain::SearchLocalisations,
            cache_key,
            Arc::clone(&result),
        );
        snapshot.query_cache().insert(
            snapshot.revision(),
            engine::CacheDomain::SearchLocalisationReuse,
            LOCALISATION_REUSE_KEY.into(),
            Arc::new(LocalisationMemo {
                corpus: Arc::clone(&result),
                catalog,
                epoch,
            }),
        );
        return Ok(result);
    }
    let membership = data
        .files
        .values()
        .enumerate()
        .map(|(index, part)| {
            (
                Arc::as_ptr(part) as usize,
                (data.effective[index], localisation_priority(snapshot, part)),
            )
        })
        .collect::<rustc_hash::FxHashMap<_, _>>();
    let retained = previous
        .as_ref()
        .map(|memo| {
            memo.corpus
                .entries
                .iter()
                .filter(|entry| membership.contains_key(&(Arc::as_ptr(&entry.data) as usize)))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let old_files = previous
        .as_ref()
        .map(|memo| {
            memo.corpus
                .files
                .values()
                .map(|part| Arc::as_ptr(part) as usize)
                .collect::<rustc_hash::FxHashSet<_>>()
        })
        .unwrap_or_default();
    let mut changed = Vec::new();
    let mut languages = BTreeSet::new();
    for part in data.files.values() {
        cancellation.checkpoint()?;
        let existing = old_files.contains(&(Arc::as_ptr(part) as usize));
        for (index, value) in part.values.iter().enumerate() {
            if !value.language.is_empty() {
                languages.insert(value.language.as_ref());
            }
            if !existing {
                changed.push(EditorLocalisationEntry {
                    data: Arc::clone(part),
                    index,
                    active: false,
                });
            }
        }
    }
    changed.sort_by(localisation_group_order);
    let mut retained = retained.into_iter().peekable();
    let mut changed = changed.into_iter().peekable();
    data.entries.reserve(retained.len() + changed.len());
    while let (Some(old), Some(new)) = (retained.peek(), changed.peek()) {
        let take_old = localisation_group_order(old, new).is_le();
        let next = if take_old {
            retained.next()
        } else {
            changed.next()
        };
        if let Some(next) = next {
            data.entries.push(next);
        }
    }
    data.entries.extend(retained);
    data.entries.extend(changed);
    data.languages = languages.into_iter().map(str::to_owned).collect();
    let mut start = 0;
    while start < data.entries.len() {
        cancellation.checkpoint()?;
        let mut end = start + 1;
        while end < data.entries.len()
            && localisation_group_order(&data.entries[start], &data.entries[end]).is_eq()
        {
            end += 1;
        }
        let group = &mut data.entries[start..end];
        let winner = (0..group.len())
            .filter(|index| membership[&(Arc::as_ptr(&group[*index].data) as usize)].0)
            .max_by(|left, right| {
                localisation_entry_rank(
                    &group[*left],
                    membership[&(Arc::as_ptr(&group[*left].data) as usize)].1,
                )
                .cmp(&localisation_entry_rank(
                    &group[*right],
                    membership[&(Arc::as_ptr(&group[*right].data) as usize)].1,
                ))
            });
        for index in 0..group.len() {
            let active = membership[&(Arc::as_ptr(&group[index].data) as usize)].0
                && winner.is_some_and(|winner| {
                    localisation_entry_rank(
                        &group[index],
                        membership[&(Arc::as_ptr(&group[index].data) as usize)].1,
                    ) == localisation_entry_rank(
                        &group[winner],
                        membership[&(Arc::as_ptr(&group[winner].data) as usize)].1,
                    )
                });
            group[index].active = active;
        }
        group.sort_by(|left, right| {
            (
                !left.active,
                left.root(),
                left.path(),
                left.key_range().start(),
            )
                .cmp(&(
                    !right.active,
                    right.root(),
                    right.path(),
                    right.key_range().start(),
                ))
        });
        start = end;
    }
    cancellation.checkpoint()?;
    static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    data.generation = NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let data = Arc::new(data);
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::SearchLocalisations,
        cache_key,
        Arc::clone(&data),
    );
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::SearchLocalisationReuse,
        LOCALISATION_REUSE_KEY.into(),
        Arc::new(LocalisationMemo {
            corpus: Arc::clone(&data),
            catalog,
            epoch,
        }),
    );
    Ok(data)
}
fn localisation_group_order(
    left: &EditorLocalisationEntry,
    right: &EditorLocalisationEntry,
) -> std::cmp::Ordering {
    (left.normalised_key(), left.language()).cmp(&(right.normalised_key(), right.language()))
}

fn localisation_entry_rank(entry: &EditorLocalisationEntry, priority: u64) -> (u64, &str, u32) {
    (
        priority,
        entry.path().map_or("", text::LogicalPath::as_str),
        entry.key_range().start(),
    )
}

fn load_localisation_file(
    snapshot: &AnalysisSnapshot,
    file: &vfs::SourceFile,
    budget: usize,
    cancellation: &CancellationToken,
) -> Result<Arc<LocalisationFile>, Cancelled> {
    let disk_backed = snapshot.file_state(file.id).is_none();
    let mut stamp = None;
    let mut notes = Vec::new();
    let input = syntax_input_for_source_file(snapshot, file.id).or_else(|| {
        let mut report = vfs::WorkspaceScanReport::default();
        let source = vfs::scan::read_source_file_with_metadata(
            &file.physical_path,
            snapshot.scan_limits(),
            &mut report,
            snapshot.game_profile().source_encoding,
        )
        .map(|(source, metadata)| {
            stamp = metadata.as_ref().map(LocalisationDiskStamp::from_metadata);
            source
        });
        notes.extend(
            report
                .issues
                .iter()
                .map(|issue| format!("{}: {}", issue.path.display(), issue.detail)),
        );
        source.map(|source| {
            let source: Arc<str> = Arc::from(source);
            ParsedInput {
                document: None,
                file: Some(file.id),
                path: Some(file.logical_path.clone()),
                format: FileFormat::Localisation,
                parsed: ParsedContent::Text(Arc::new(parser::parse(
                    FileFormat::Localisation,
                    &source,
                ))),
                source,
                hir: None,
                profile: snapshot.game_profile_handle(),
            }
        })
    });
    let mut part = match input {
        Some(input) => collect_localisations(snapshot, &input, budget, cancellation)?,
        None => {
            notes.push(format!("{}: full localisation values unavailable; restore source files or refresh the index", file.logical_path.as_str()));
            LocalisationFile {
                source: Arc::from(""),
                descriptor: None,
                disk_backed,
                disk_stamp: None,
                document: None,
                physical: Some(file.physical_path.clone()),
                file: Some(file.id),
                path: Some(file.logical_path.clone()),
                root: Some(file.root_id),
                version: None,
                values: Vec::new(),
                limitations: Vec::new(),
                retained_bytes: 0,
                budget_reached: false,
            }
        }
    };
    part.descriptor = Some(file.clone());
    part.disk_backed = disk_backed;
    part.disk_stamp = stamp;
    part.limitations.extend(notes);
    Ok(Arc::new(part))
}

fn retain_localisation_file(
    data: &mut EditorLocalisations,
    key: LocalisationInput,
    part: Arc<LocalisationFile>,
) {
    if data.retained_bytes.saturating_add(part.retained_bytes) > LOCALISATION_BYTES {
        data.budget_reached = true;
        data.limitations.push("Full localisation corpus exceeded the 256 MiB search budget; results are partial. Reduce the configured roots or languages in the source data.".to_owned());
        return;
    }
    data.retained_bytes += part.retained_bytes;
    data.budget_reached |= part.budget_reached;
    data.limitations.extend(part.limitations.iter().cloned());
    data.files.insert(key, part);
}
fn localisation_priority(snapshot: &AnalysisSnapshot, file: &LocalisationFile) -> u64 {
    file.root
        .and_then(|id| snapshot.source_roots().iter().find(|root| root.id == id))
        .map_or(0, vfs::root_priority)
}
fn collect_localisations(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    budget: usize,
    cancellation: &CancellationToken,
) -> Result<LocalisationFile, Cancelled> {
    let root = input
        .file
        .and_then(|id| snapshot.source_files().get(&id).map(|file| file.root_id))
        .or_else(|| {
            input
                .document
                .as_ref()
                .and_then(|id| snapshot.document(id))
                .and_then(|document| document.path())
                .and_then(|path| {
                    snapshot
                        .source_roots()
                        .iter()
                        .filter(|root| path.starts_with(&root.path))
                        .max_by_key(|root| root.path.as_os_str().len())
                        .map(|root| root.id)
                })
        });
    let document = input.document.as_ref().and_then(|id| snapshot.document(id));
    let mut data = LocalisationFile {
        source: Arc::clone(&input.source),
        descriptor: None,
        disk_backed: false,
        disk_stamp: None,
        physical: document
            .and_then(|document| document.path())
            .map(text::AbsPath::normalize),
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        root,
        version: document.and_then(|document| document.version()),
        values: Vec::new(),
        limitations: Vec::new(),
        retained_bytes: input.source.len()
            + std::mem::size_of::<LocalisationFile>()
            + input.path.as_ref().map_or(0, |path| path.as_str().len()),
        budget_reached: false,
    };
    if unreliable_encoded_text(&input.source) {
        data.limitations.push("An editor buffer contains replacement characters alongside encoded text; decoded matching is limited. Open a reliable decoded view before searching that content.".to_owned());
    }
    let ParsedContent::Text(parsed) = &input.parsed;
    let mut language: Arc<str> = Arc::from("");
    let mut missing_value = false;
    let mut missing_language = false;
    for node in parsed.root().children() {
        cancellation.checkpoint()?;
        if node.kind() == CstKind::LanguageHeader {
            let header = input
                .source_text(node.range())
                .unwrap_or("")
                .trim()
                .trim_end_matches(':');
            language = Arc::from(
                header
                    .strip_prefix("l_")
                    .unwrap_or(header)
                    .to_ascii_lowercase(),
            );
            data.retained_bytes += language.len();
            continue;
        }
        if node.kind() != CstKind::LocalisationEntry {
            continue;
        }
        let key = node
            .children()
            .find(|child| child.kind() == CstKind::LocalisationKey);
        let value = node.children().find(|child| {
            matches!(
                child.kind(),
                CstKind::LocalisationString | CstKind::UnquotedValue
            )
        });
        let (Some(key), Some(value)) = (key, value) else {
            missing_value = true;
            continue;
        };
        if language.is_empty() {
            missing_language = true;
        }
        let raw = input.source_text(value.range()).unwrap_or("");
        let unquoted = raw.strip_prefix('"').unwrap_or(raw);
        let unquoted = unquoted.strip_suffix('"').unwrap_or(unquoted);
        let start = value.range().start() + u32::from(raw.starts_with('"'));
        let readable_range =
            text::TextRange::new(start, start + unquoted.len() as u32).unwrap_or(value.range());
        let value_unreliable = unreliable_encoded_text(unquoted);
        let transformed = value_unreliable
            || unquoted.contains('§')
            || unquoted.contains("\\n")
            || unquoted.bytes().any(|byte| (0x10..=0x13).contains(&byte));
        let readable = transformed
            .then(|| {
                readable_localisation(&if value_unreliable {
                    recover_unreliable_text(unquoted)
                } else {
                    transcode::decode_value(unquoted)
                })
            })
            .filter(|readable| readable != unquoted);
        let spelling = input.source_text(key.range()).unwrap_or("");
        let folded_key = spelling
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
            .then(|| spelling.to_ascii_lowercase().into_boxed_str());
        let added = std::mem::size_of::<LocalisationValue>()
            + std::mem::size_of::<EditorLocalisationEntry>()
            + readable.as_ref().map_or(0, String::len)
            + folded_key.as_ref().map_or(0, |key| key.len());
        if data.retained_bytes.saturating_add(added) > budget {
            data.budget_reached = true;
            data.limitations.push("Full localisation corpus exceeded the 256 MiB search budget; results are partial. Reduce the configured roots or languages in the source data.".to_owned());
            break;
        }
        data.retained_bytes += added;
        data.values.push(LocalisationValue {
            key_range: key.range(),
            value_range: value.range(),
            readable_range,
            folded_key,
            readable,
            language: Arc::clone(&language),
            full_value_available: !value_unreliable,
        });
    }
    if missing_value {
        data.limitations.push(format!(
            "{}: some localisation entries have no complete parsed value",
            input.path.as_ref().map_or("document", |path| path.as_str())
        ));
    }
    if missing_language {
        data.limitations.push("Some localisation entries have no parsed language header; language coverage is incomplete.".to_owned());
    }
    Ok(data)
}
/// Remove known display controls while preserving runtime placeholders verbatim.
#[must_use]
pub fn readable_localisation(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '§'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphanumeric() || *next == '!')
        {
            chars.next();
        } else if character == '\\' && chars.peek() == Some(&'n') {
            chars.next();
            result.push('\n');
        } else {
            result.push(character);
        }
    }
    result
}

/// Query words are normalised once; ASCII values require no temporary folded string.
pub struct LocalisationQuery {
    words: Vec<String>,
}

impl LocalisationQuery {
    pub fn new(query: &str) -> Self {
        Self {
            words: query.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn matches(&self, value: &str) -> bool {
        if self.words.is_empty() {
            return false;
        }
        if value.is_ascii() {
            return self.words.iter().all(|word| {
                word.is_ascii()
                    && word.len() <= value.len()
                    && value
                        .as_bytes()
                        .windows(word.len())
                        .any(|part| part.eq_ignore_ascii_case(word.as_bytes()))
            });
        }
        let folded = value.to_lowercase();
        self.words.iter().all(|word| folded.contains(word))
    }
}

/// Multi-keyword matching searches the complete readable value, never a preview.
#[must_use]
pub fn localisation_matches(value: &str, query: &str) -> bool {
    LocalisationQuery::new(query).matches(value)
}

/// Known language identities come from entry headers, rather than path or glyph guesses.
#[must_use]
pub fn editor_localisation_languages(data: &EditorLocalisations) -> Vec<String> {
    data.languages.clone()
}

/// A retained definition version, including definitions hidden by another source.
#[derive(Clone, Debug)]
pub struct EditorDefinition {
    pub name: String,
    pub kind: String,
    pub location: Location,
    pub root: Option<SourceRootId>,
    pub version: Option<i64>,
    pub active: bool,
    definition_range: text::TextRange,
    priority: u64,
}

/// Enumerate all versions before matching or applying source filters.
pub fn editor_definitions(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<Vec<EditorDefinition>>, Cancelled> {
    cancellation.checkpoint()?;
    let key = format!("editor-definitions:{}", snapshot.revision());
    if let Some(data) = snapshot.query_cache().get(snapshot.revision(), &key) {
        return Ok(data);
    }
    use crate::resolution::{
        definition_priority, definition_priority_for_file, resolution_order,
        semantic_data_with_cancellation,
    };
    let hidden = crate::support::overlay_file_ids(snapshot);
    let suppressed = twin_suppressed_overlays(snapshot);
    let mut result = Vec::new();
    for definition in snapshot.index().definitions_iter() {
        cancellation.checkpoint()?;
        if definition.kind.eq_ignore_ascii_case("localisation")
            || hidden.contains(&definition.file_id)
        {
            continue;
        }
        let file = snapshot.source_files().get(&definition.file_id);
        result.push(EditorDefinition {
            name: definition.name.to_string(),
            kind: definition.kind.to_string(),
            location: Location {
                document: None,
                file: Some(definition.file_id),
                path: file.map(|file| file.logical_path.clone()),
                range: definition.selection_range,
            },
            root: file.map(|file| file.root_id),
            version: None,
            active: false,
            definition_range: definition.range,
            priority: definition_priority_for_file(snapshot, definition.file_id),
        });
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay || suppressed.contains(document.id()) {
            continue;
        }
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        for definition in
            semantic_data_with_cancellation(snapshot, &input, cancellation)?.definitions
        {
            if definition.kind.eq_ignore_ascii_case("localisation") {
                continue;
            }
            let root = definition
                .file
                .and_then(|id| snapshot.source_files().get(&id).map(|file| file.root_id))
                .or_else(|| {
                    document.path().and_then(|path| {
                        snapshot
                            .source_roots()
                            .iter()
                            .filter(|root| path.starts_with(&root.path))
                            .max_by_key(|root| root.path.as_os_str().len())
                            .map(|root| root.id)
                    })
                });
            let priority = definition_priority(snapshot, &definition);
            result.push(EditorDefinition {
                name: definition.name,
                kind: definition.kind,
                root,
                version: document.version(),
                active: false,
                definition_range: definition.symbol.location.range,
                priority,
                location: Location {
                    range: definition.symbol.selection_range,
                    ..definition.symbol.location
                },
            });
        }
    }
    // Resolve all identities in one pass with the resolver's path/range ordering.
    // Stable equal-rank candidates put index entries after overlays, as DirectResolutionContext does.
    let keys = result
        .iter()
        .map(|entry| {
            (
                entry.kind.to_ascii_lowercase(),
                entry.name.to_ascii_lowercase(),
            )
        })
        .collect::<Vec<_>>();
    let mut winners = rustc_hash::FxHashMap::<&(String, String), usize>::default();
    let mut effective_files = rustc_hash::FxHashMap::default();
    for (index, entry) in result.iter().enumerate() {
        cancellation.checkpoint()?;
        let effective = if let Some(document) = &entry.location.document {
            snapshot.source_is_effective(Some(document), None)
        } else if let Some(file) = entry.location.file {
            *effective_files
                .entry(file)
                .or_insert_with(|| snapshot.source_is_effective(None, Some(file)))
        } else {
            false
        };
        if !effective {
            continue;
        }
        winners
            .entry(&keys[index])
            .and_modify(|previous| {
                let previous_entry = &result[*previous];
                let order = resolution_order(
                    entry.priority,
                    entry.location.path.as_ref(),
                    entry.definition_range,
                    entry.location.range,
                )
                .cmp(&resolution_order(
                    previous_entry.priority,
                    previous_entry.location.path.as_ref(),
                    previous_entry.definition_range,
                    previous_entry.location.range,
                ))
                .then_with(|| {
                    entry
                        .location
                        .document
                        .is_none()
                        .cmp(&previous_entry.location.document.is_none())
                });
                if order.is_ge() {
                    *previous = index;
                }
            })
            .or_insert(index);
    }
    let active = result
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            winners.get(&keys[index]).is_some_and(|winner| {
                let winner = &result[*winner];
                entry.location.document == winner.location.document
                    && entry.location.file == winner.location.file
                    && entry.location.range == winner.location.range
            })
        })
        .collect::<Vec<_>>();
    for (entry, active) in result.iter_mut().zip(active) {
        entry.active = active;
    }
    let result = Arc::new(result);
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Documents,
        key,
        Arc::clone(&result),
    );
    Ok(result)
}

/// Metadata uses index type buckets and authoritative overlays, without enumerating search results.
pub fn editor_definition_kinds(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, Cancelled> {
    let mut kinds = snapshot
        .index()
        .definition_kinds()
        .filter(|kind| !kind.eq_ignore_ascii_case("localisation"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let suppressed = snapshot.suppressed_overlay_documents();
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay || suppressed.contains(document.id()) {
            continue;
        }
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        for definition in
            crate::resolution::semantic_data_with_cancellation(snapshot, &input, cancellation)?
                .definitions
        {
            if !definition.kind.eq_ignore_ascii_case("localisation") {
                kinds.insert(definition.kind);
            }
        }
    }
    Ok(kinds.into_iter().collect())
}

/// Name matching follows the definition browser's exact/prefix/substring/subsequence order.
#[must_use]
pub fn editor_name_score(name: &str, query: &str) -> Option<u8> {
    let name = name.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    if query.is_empty() {
        return None;
    }
    if name == query {
        return Some(0);
    }
    if name.starts_with(&query) {
        return Some(1);
    }
    if name.contains(&query) {
        return Some(2);
    }
    let mut remaining = query.chars();
    let mut next = remaining.next();
    for character in name.chars() {
        if next == Some(character) {
            next = remaining.next();
        }
    }
    next.is_none().then_some(3)
}

/// A full-text hit carries text from the fixed corpus for UTF-16 conversion at the boundary.
#[derive(Clone, Debug)]
pub struct EditorTextHit {
    pub root: SourceRootId,
    pub path: text::LogicalPath,
    pub physical_path: text::AbsPath,
    pub document: Option<engine::DocumentId>,
    pub version: Option<i64>,
    pub text: Arc<str>,
    pub range: text::TextRange,
    pub preview: String,
    pub context: String,
}

#[derive(Clone, Debug, Default)]
pub struct EditorTextResults {
    pub generation: u64,
    pub hits: Vec<EditorTextHit>,
    pub limitations: Vec<String>,
}

/// Literal text search across readable source-root files, including comments and unclassified
/// text. Glob filters use the VFS configuration matcher with exclude taking precedence.
pub fn editor_text_search(
    snapshot: &AnalysisSnapshot,
    query: &str,
    roots: Option<&[u32]>,
    case_sensitive: bool,
    include: &[String],
    exclude: &[String],
    cancellation: &CancellationToken,
) -> Result<EditorTextResults, Cancelled> {
    editor_text_search_query(
        snapshot,
        &EditorTextQuery {
            query,
            roots,
            case_sensitive,
            include,
            exclude,
            epoch: 0,
            buffers: &[],
        },
        cancellation,
    )
}

#[derive(Clone, Debug)]
pub struct EditorTextBuffer {
    pub document: engine::DocumentId,
    pub path: text::AbsPath,
    pub version: Option<i64>,
    pub text: Arc<str>,
}

pub struct EditorTextQuery<'a> {
    pub query: &'a str,
    pub roots: Option<&'a [u32]>,
    pub case_sensitive: bool,
    pub include: &'a [String],
    pub exclude: &'a [String],
    pub epoch: u64,
    pub buffers: &'a [EditorTextBuffer],
}

pub fn editor_text_search_query(
    snapshot: &AnalysisSnapshot,
    options: &EditorTextQuery<'_>,
    cancellation: &CancellationToken,
) -> Result<EditorTextResults, Cancelled> {
    let EditorTextQuery {
        query,
        roots,
        case_sensitive,
        include,
        exclude,
        epoch,
        buffers,
    } = *options;
    cancellation.checkpoint()?;
    let include = vfs::WorkspaceScanFilters::new(include.to_vec(), Vec::new());
    let exclude = vfs::WorkspaceScanFilters::new(exclude.to_vec(), Vec::new());
    let (Ok(include), Ok(exclude)) = (include, exclude) else {
        return Ok(EditorTextResults {
            generation: 0,
            hits: Vec::new(),
            limitations: vec!["invalid path filter".to_owned()],
        });
    };
    let corpus = snapshot
        .search_texts(roots, epoch, &include, &exclude, &|| {
            cancellation.is_cancelled()
        })
        .map_err(|_| Cancelled)?;
    let mut result = EditorTextResults {
        generation: corpus.generation,
        hits: Vec::new(),
        limitations: corpus.limitations.clone(),
    };
    let selected = |root: SourceRootId, path: &text::LogicalPath| {
        roots.is_none_or(|roots| roots.contains(&root.get()))
            && (include.ignore_file_patterns().is_empty() || include.ignores_file(path.as_str()))
            && !exclude.ignores_file(path.as_str())
            && snapshot.text_search_path_allowed(path)
    };
    let mut overlays: BTreeMap<text::AbsPath, EditorTextBuffer> = BTreeMap::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        cancellation.checkpoint()?;
        let Some(path) = document.path() else {
            continue;
        };
        let buffer = EditorTextBuffer {
            document: document.id().clone(),
            path: text::AbsPath::normalize(path),
            version: document.version(),
            text: document.text_handle(),
        };
        insert_text_buffer(&mut overlays, buffer);
    }
    for buffer in buffers {
        cancellation.checkpoint()?;
        insert_text_buffer(&mut overlays, buffer.clone());
    }
    for file in &corpus.files {
        cancellation.checkpoint()?;
        if !selected(file.root, &file.path) || overlays.contains_key(&file.physical_path) {
            continue;
        }
        text_hits(
            &mut result,
            file.root,
            &file.path,
            &file.physical_path,
            None,
            None,
            Arc::clone(&file.text),
            query,
            case_sensitive,
            cancellation,
        )?;
    }
    for (path, buffer) in overlays {
        cancellation.checkpoint()?;
        let Some(root) = snapshot
            .source_roots()
            .iter()
            .filter(|root| path.starts_with(&root.path))
            .max_by_key(|root| root.path.as_os_str().len())
        else {
            continue;
        };
        let Some(logical) = path
            .strip_prefix(&root.path)
            .ok()
            .and_then(|relative| text::LogicalPath::parse(&relative.to_string_lossy()).ok())
        else {
            continue;
        };
        if !selected(root.id, &logical) {
            continue;
        }
        text_hits(
            &mut result,
            root.id,
            &logical,
            &path,
            Some(buffer.document),
            buffer.version,
            buffer.text,
            query,
            case_sensitive,
            cancellation,
        )?;
    }
    result.hits.sort_by(|left, right| {
        (left.root, &left.path, left.range.start()).cmp(&(
            right.root,
            &right.path,
            right.range.start(),
        ))
    });
    Ok(result)
}

fn insert_text_buffer(
    overlays: &mut BTreeMap<text::AbsPath, EditorTextBuffer>,
    buffer: EditorTextBuffer,
) {
    if overlays.get(&buffer.path).is_none_or(|current| {
        current.document == buffer.document
            || engine::prefer_overlay_document(&current.document, &buffer.document)
    }) {
        overlays.insert(buffer.path.clone(), buffer);
    }
}

#[allow(clippy::too_many_arguments)]
fn text_hits(
    result: &mut EditorTextResults,
    root: SourceRootId,
    path: &text::LogicalPath,
    physical_path: &text::AbsPath,
    document: Option<engine::DocumentId>,
    version: Option<i64>,
    source: Arc<str>,
    query: &str,
    case_sensitive: bool,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    let unreliable = unreliable_encoded_text(&source);
    if unreliable {
        result.limitations.push(format!(
            "{}: replacement characters alongside encoded text prevent reliable decoded matching",
            path.as_str()
        ));
    }
    // Case folding retains a byte-to-source map, including characters whose lowercase expands.
    let identity_mapping = !source.bytes().any(|byte| (0x10..=0x13).contains(&byte))
        && (case_sensitive || source.is_ascii());
    let mut mapping = Vec::new();
    let folded = if identity_mapping {
        if case_sensitive {
            std::borrow::Cow::Borrowed(source.as_ref())
        } else {
            std::borrow::Cow::Owned(source.to_ascii_lowercase())
        }
    } else {
        let mut folded = String::new();
        let mut characters = source.char_indices().peekable();
        let mut quoted = false;
        let mut comment = false;
        let mut escaped = false;
        while let Some((offset, character)) = characters.next() {
            if offset & 1023 == 0 {
                cancellation.checkpoint()?;
            }
            let mut source_end = offset + character.len_utf8();
            if character == '\n' {
                comment = false;
            }
            if !quoted && character == '#' {
                comment = true;
            }
            if !comment && character == '"' && !escaped {
                quoted = !quoted;
            }
            if quoted && (0x10..=0x13).contains(&u32::from(character)) {
                for _ in 0..2 {
                    if let Some((next, character)) = characters.next() {
                        source_end = next + character.len_utf8();
                    }
                }
                let readable = if unreliable {
                    "\u{fffc}".to_owned()
                } else {
                    transcode::decode_value(source.get(offset..source_end).unwrap_or(""))
                };
                let run = if case_sensitive {
                    readable
                } else {
                    readable.to_lowercase()
                };
                mapping.extend(std::iter::repeat_n((offset, source_end), run.len()));
                folded.push_str(&run);
            } else {
                let start = folded.len();
                if case_sensitive {
                    folded.push(character);
                } else {
                    folded.extend(character.to_lowercase());
                }
                mapping.extend(std::iter::repeat_n(
                    (offset, source_end),
                    folded.len() - start,
                ));
            }
            escaped = character == '\\' && !escaped;
        }
        std::borrow::Cow::Owned(folded)
    };
    let query = if case_sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    if query.is_empty() {
        return Ok(());
    }
    let preview_query = query.to_lowercase();
    for (start, _) in folded.match_indices(query.as_str()) {
        cancellation.checkpoint()?;
        if result.hits.len() >= 10_000 {
            if !result
                .limitations
                .iter()
                .any(|line| line.starts_with("Text result budget"))
            {
                result.limitations.push(
                    "Text result budget reached (10000 locations); narrow the query".to_owned(),
                );
            }
            break;
        }
        let end = start + query.len();
        let (source_start, source_end) = if identity_mapping {
            (start, end)
        } else {
            let (Some((begin, _)), Some((_, finish))) = (mapping.get(start), mapping.get(end - 1))
            else {
                continue;
            };
            (*begin, *finish)
        };
        let line_start = source[..source_start]
            .rfind('\n')
            .map_or(0, |offset| offset + 1);
        let line_end = source[source_end..]
            .find('\n')
            .map_or(source.len(), |offset| source_end + offset);
        let readable = if unreliable {
            recover_unreliable_text(&source[line_start..line_end])
        } else {
            transcode::decode_value(&source[line_start..line_end])
        };
        let folded_preview = readable.to_lowercase();
        let mut boundary = folded_preview
            .find(&preview_query)
            .unwrap_or(0)
            .min(readable.len());
        while !readable.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let match_offset = readable[..boundary].chars().count();
        let preview = readable
            .chars()
            .skip(match_offset.saturating_sub(120))
            .take(1000)
            .collect::<String>();
        let mut context_start = line_start;
        let mut context_end = line_end;
        for _ in 0..2 {
            context_start = source[..context_start.saturating_sub(1)]
                .rfind('\n')
                .map_or(0, |offset| offset + 1);
            context_end = if context_end >= source.len() {
                source.len()
            } else {
                source[context_end + 1..]
                    .find('\n')
                    .map_or(source.len(), |offset| context_end + 1 + offset)
            };
        }
        let context = if unreliable {
            recover_unreliable_text(&source[context_start..context_end])
        } else {
            transcode::decode_value(&source[context_start..context_end])
        }
        .chars()
        .take(2000)
        .collect::<String>();
        let (Ok(start), Ok(end)) = (u32::try_from(source_start), u32::try_from(source_end)) else {
            continue;
        };
        result.hits.push(EditorTextHit {
            root,
            path: path.clone(),
            physical_path: physical_path.clone(),
            document: document.clone(),
            version,
            text: Arc::clone(&source),
            range: text::TextRange::new(start, end)
                .unwrap_or_else(|| text::TextRange::empty(start)),
            preview,
            context,
        });
    }
    Ok(())
}

/// Locate the first query word in the readable value and map it to the original document,
/// including display controls and EU4dll escape triples crossed by the match.
#[must_use]
pub fn editor_localisation_match_location(
    entry: &EditorLocalisationEntry,
    query: &str,
) -> Option<Location> {
    let raw_start = entry.value_range().start() as usize;
    let raw_end = entry.value_range().end() as usize;
    let raw = entry.source().get(raw_start..raw_end)?;
    let quoted = raw.starts_with('"');
    let start = raw_start + usize::from(quoted);
    let end = raw_end - usize::from(quoted && raw.ends_with('"'));
    let raw = entry.source().get(start..end)?;
    if entry.record().readable.is_none() && raw.is_ascii() {
        let word = query.split_whitespace().next()?.to_lowercase();
        let offset = raw.to_ascii_lowercase().find(&word)?;
        return Some(Location {
            range: text::TextRange::new(
                u32::try_from(start + offset).ok()?,
                u32::try_from(start + offset + word.len()).ok()?,
            )?,
            ..entry.location()
        });
    }
    let mut units = Vec::new();
    let mut chars = raw.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        let mut end = offset + character.len_utf8();
        let decoded = if (0x10..=0x13).contains(&u32::from(character)) {
            for _ in 0..2 {
                if let Some((next, character)) = chars.next() {
                    end = next + character.len_utf8();
                }
            }
            transcode::decode_value(raw.get(offset..end)?)
        } else {
            character.to_string()
        };
        for character in decoded.chars() {
            units.push((character, start + offset, start + end));
        }
    }
    let mut folded = String::new();
    let mut positions = Vec::new();
    let mut index = 0;
    while index < units.len() {
        let (character, begin, end) = units[index];
        if character == '§'
            && units
                .get(index + 1)
                .is_some_and(|(next, _, _)| next.is_ascii_alphanumeric() || *next == '!')
        {
            index += 2;
            continue;
        }
        let (character, end) = if character == '\\'
            && units
                .get(index + 1)
                .is_some_and(|(next, _, _)| *next == 'n')
        {
            index += 1;
            ('\n', units[index].2)
        } else {
            (character, end)
        };
        let folded_character = character.to_lowercase().collect::<String>();
        for _ in 0..folded_character.len() {
            positions.push((begin, end));
        }
        folded.push_str(&folded_character);
        index += 1;
    }
    let word = query.split_whitespace().next()?.to_lowercase();
    let offset = folded.find(&word)?;
    let begin = positions.get(offset)?.0;
    let end = positions.get(offset + word.len() - 1)?.1;
    let range = text::TextRange::new(u32::try_from(begin).ok()?, u32::try_from(end).ok()?)?;
    Some(Location {
        range,
        ..entry.location()
    })
}

/// Cheap language choices from already loaded headers. Opening an empty panel never
/// materializes the full-value corpus or reads source roots from disk.
#[must_use]
pub fn editor_known_localisation_languages(snapshot: &AnalysisSnapshot) -> Vec<String> {
    let key = format!("editor-localisations:{}:0", snapshot.revision());
    if let Some(data) = snapshot
        .query_cache()
        .get::<EditorLocalisations>(snapshot.revision(), &key)
    {
        return editor_localisation_languages(&data);
    }
    let mut languages = BTreeSet::new();
    let hidden = crate::support::overlay_file_ids(snapshot);
    let suppressed = snapshot.suppressed_overlay_documents();
    for id in snapshot
        .source_files()
        .keys()
        .filter(|id| !hidden.contains(id))
    {
        if let Some((_, preview)) = snapshot.localisation_previews().file_entries(*id).first()
            && let Some(language) = preview.language.as_deref()
        {
            languages.insert(
                language
                    .strip_prefix("l_")
                    .unwrap_or(language)
                    .to_ascii_lowercase(),
            );
        }
    }
    for source in snapshot
        .source_files()
        .keys()
        .filter(|id| !hidden.contains(id))
        .filter_map(|id| snapshot.source_text(*id))
        .chain(
            snapshot
                .documents()
                .values()
                .filter(|document| {
                    document.source() == DocumentSource::Overlay
                        && !suppressed.contains(document.id())
                })
                .map(|document| document.text()),
        )
    {
        for line in source.lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if let Some(language) = line
                .strip_prefix("l_")
                .and_then(|line| line.strip_suffix(':'))
                && language
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                languages.insert(language.to_ascii_lowercase());
            }
        }
    }
    languages.into_iter().collect()
}

#[derive(Clone, Debug)]
pub struct EditorRule {
    pub id: String,
    pub name: String,
    pub context: String,
    pub documentation: String,
    pub allowed_scopes: Vec<String>,
    pub push_scope: Option<String>,
    pub value_requirement: String,
    pub minimum: u32,
    pub maximum: Option<u32>,
    pub deprecated: bool,
}

/// Query the current first-party IR, retaining same-named contracts in distinct schemas
/// and discoverable pattern keys. Scope filtering includes unrestricted declarations.
pub fn editor_rule_search(
    snapshot: &AnalysisSnapshot,
    query: &str,
    context: Option<&str>,
    scope: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<Vec<EditorRule>, Cancelled> {
    let ir = snapshot.ir();
    let query = query.to_lowercase();
    let mut result = Vec::new();
    for (schema_index, schema) in ir.schemas.iter().enumerate() {
        cancellation.checkpoint()?;
        let name = ir.strings.resolve(schema.name);
        if context.is_some_and(|context| context != name) {
            continue;
        }
        let mut fields = schema
            .exact
            .values()
            .flatten()
            .copied()
            .chain(schema.patterns.iter().copied())
            .collect::<Vec<_>>();
        fields.sort_by_key(|field| field.index());
        fields.dedup();
        for field_id in fields {
            cancellation.checkpoint()?;
            let field = ir.field(field_id);
            let label = match ir.matcher(field.key) {
                rules::ir::Matcher::Literal(value) => ir.strings.resolve(*value).to_owned(),
                _ => crate::ir_matcher_description(ir, field.key),
            };
            let doc = field.doc.map(|doc| ir.strings.resolve(doc)).unwrap_or("");
            let allowed = field
                .scope
                .as_ref()
                .map(|scope| {
                    scope
                        .scopes_in
                        .iter()
                        .map(|name| ir.strings.resolve(*name).to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if (!label.to_lowercase().contains(&query) && !doc.to_lowercase().contains(&query))
                || scope.is_some_and(|scope| {
                    !allowed.is_empty() && !allowed.iter().any(|allowed| allowed == scope)
                })
            {
                continue;
            }
            let requirement = match field.value {
                rules::ir::FieldValue::Scalar(matcher) => {
                    crate::ir_matcher_description(ir, matcher)
                }
                rules::ir::FieldValue::Block(schema) => {
                    format!("block: {}", ir.strings.resolve(ir.schema(schema).name))
                }
                rules::ir::FieldValue::SelfBlock => format!("block: {name}"),
            };
            result.push(EditorRule {
                id: format!("ir:{schema_index}:{}", field_id.index()),
                name: label,
                context: name.to_owned(),
                documentation: doc.to_owned(),
                allowed_scopes: allowed,
                push_scope: field
                    .scope
                    .as_ref()
                    .and_then(|scope| scope.push)
                    .map(|name| ir.strings.resolve(name).to_owned()),
                value_requirement: requirement,
                minimum: field.card.min,
                maximum: field.card.max,
                deprecated: field.deprecated,
            });
        }
    }
    Ok(result)
}

/// Resolve a declared localisation binding for a result title without changing name matching.
pub fn editor_definition_title(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
    let Some(key) = snapshot.localisation_template_key(kind, "name", name) else {
        return Ok(None);
    };
    let values = crate::localisation_values_by_key(snapshot, &[key.as_str()], cancellation)?;
    Ok(values.get(&key).map(|(_, value)| value.clone()))
}

fn unreliable_encoded_text(source: &str) -> bool {
    source.contains('\u{fffd}')
        && source
            .chars()
            .any(|character| (0x10..=0x13).contains(&u32::from(character)))
}

fn recover_unreliable_text(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(character) = chars.next() {
        if (0x10..=0x13).contains(&u32::from(character)) {
            chars.next();
            chars.next();
            result.push('\u{fffc}');
        } else {
            result.push(character);
        }
    }
    result
}
