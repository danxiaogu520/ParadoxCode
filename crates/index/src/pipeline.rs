//! Parse, lower, and materialize immutable per-file pipeline state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use parser::{FileFormat, ParsedFile, parse};
use rules::ir::{DocumentParser, RulesIr, SymbolFacts};
use rules::{GameProfile, ParserKind, RuleSet};
use text::{AbsPath, LineIndex, LogicalPath, PositionRange, TextRange};

use crate::documents::{DocumentSnapshot, FileState, ParsedSource};
use crate::index::{
    Definition, DynamicDefinitionSummary, DynamicParameterSignature, FileIndexShard, Reference,
};
use crate::{record_pipeline_lower, record_pipeline_parse};
use hir::{
    HirFile, lower_shared, lower_shared_for_index_with_ir, lower_shared_with_ir,
    lower_shared_with_ir_and_facts, lower_shared_with_profile,
};
use vfs::ParseCache;
use vfs::read_source_file_cancellable;
use vfs::{
    DocumentId, DocumentSource, SourceFile, SourceFileId, SourceRoot, WorkspaceError,
    WorkspaceScanLimits, WorkspaceScanReport, WorkspaceScanToken,
};

/// IR workspace predicates backed by a candidate index and uncommitted overlays.
/// This deliberately owns no IDE state, so disk scans and cache rebuilds can use it.
pub struct IndexSymbolFacts<'a> {
    ir: &'a RulesIr,
    index: &'a crate::WorkspaceIndex,
    overlays: &'a [Arc<HirFile>],
    excluded_file_ids: Option<&'a BTreeSet<SourceFileId>>,
    templates: OnceLock<BTreeMap<String, crate::FlagWriteIndex>>,
}

impl<'a> IndexSymbolFacts<'a> {
    #[must_use]
    pub fn new(
        ir: &'a RulesIr,
        index: &'a crate::WorkspaceIndex,
        overlays: &'a [Arc<HirFile>],
    ) -> Self {
        Self {
            ir,
            index,
            overlays,
            excluded_file_ids: None,
            templates: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn with_overlay_files(
        ir: &'a RulesIr,
        index: &'a crate::WorkspaceIndex,
        overlays: &'a [Arc<HirFile>],
        excluded_file_ids: &'a BTreeSet<SourceFileId>,
    ) -> Self {
        Self {
            ir,
            index,
            overlays,
            excluded_file_ids: Some(excluded_file_ids),
            templates: OnceLock::new(),
        }
    }

    fn has_member(&self, kind: &str, name: &str) -> bool {
        self.index
            .definitions_with_state(kind, name)
            .iter()
            .any(|(definition, active)| {
                *active
                    && self
                        .excluded_file_ids
                        .is_none_or(|files| !files.contains(&definition.file_id))
            })
            || self.overlays.iter().any(|hir| {
                hir.definitions().iter().any(|definition| {
                    definition.kind.eq_ignore_ascii_case(kind)
                        && definition.name.eq_ignore_ascii_case(name)
                })
            })
            || self.template_member(kind, name)
    }

    fn template_member(&self, kind: &str, name: &str) -> bool {
        let templates = self.templates.get_or_init(|| {
            let mut templates = BTreeMap::<String, crate::FlagWriteIndex>::new();
            for (definition, active) in self.index.definition_identities() {
                if active
                    && definition.name.contains('$')
                    && self
                        .excluded_file_ids
                        .is_none_or(|files| !files.contains(&definition.file_id))
                {
                    templates
                        .entry(definition.kind.to_ascii_lowercase())
                        .or_default()
                        .record(&definition.name);
                }
            }
            for hir in self.overlays {
                for definition in hir
                    .definitions()
                    .iter()
                    .filter(|definition| definition.name.contains('$'))
                {
                    templates
                        .entry(definition.kind.to_ascii_lowercase())
                        .or_default()
                        .record(&definition.name);
                }
            }
            templates
        });
        templates
            .get(&kind.to_ascii_lowercase())
            .is_some_and(|patterns| {
                patterns.membership(name) != crate::FlagWriteMembership::Unknown
            })
    }

    fn has_subtype(&self, kind: &str, instance: &str, subtype: &str) -> bool {
        self.index
            .definitions_with_state(kind, instance)
            .into_iter()
            .filter(|(definition, active)| {
                *active
                    && self
                        .excluded_file_ids
                        .is_none_or(|files| !files.contains(&definition.file_id))
            })
            .any(|(definition, _)| {
                self.index
                    .shards
                    .get(&definition.file_id)
                    .is_some_and(|shard| {
                        shard.definition_attributes.iter().any(|attrs| {
                            attrs.definition_range == definition.range
                                && attrs.kind.eq_ignore_ascii_case(kind)
                                && attrs.name.eq_ignore_ascii_case(instance)
                                && attrs
                                    .subtypes
                                    .iter()
                                    .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
                        })
                    })
            })
            || self.overlays.iter().any(|hir| {
                hir.definition_attributes().iter().any(|attrs| {
                    attrs.kind.eq_ignore_ascii_case(kind)
                        && attrs.name.eq_ignore_ascii_case(instance)
                        && attrs
                            .subtypes
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
                })
            })
    }
}

impl rules::ir::SymbolFacts for IndexSymbolFacts<'_> {
    fn replacement_template(
        &self,
        type_id: rules::ir::TypeId,
        name: &str,
    ) -> Option<Arc<rules::replacement::Template>> {
        let kind = self.ir.strings.resolve(self.ir.type_info(type_id).name);
        let mut overlay = None;
        for hir in self.overlays {
            for definition in hir.definitions().iter().filter(|definition| {
                definition.kind.eq_ignore_ascii_case(kind)
                    && definition.name.eq_ignore_ascii_case(name)
            }) {
                if overlay.is_some() {
                    return None;
                }
                overlay = Some(hir.dynamic_template(kind, name, definition.range));
            }
        }
        if let Some(template) = overlay {
            return template.cloned().map(Arc::new);
        }
        let definition = self.index.active_definition(kind, name)?;
        if self
            .excluded_file_ids
            .is_some_and(|files| files.contains(&definition.file_id))
        {
            return None;
        }
        self.index
            .active_dynamic_definition(kind, name)?
            .template
            .clone()
            .map(Arc::new)
    }

    fn type_member(&self, type_id: rules::ir::TypeId, name: &str) -> bool {
        let ty = self.ir.type_info(type_id);
        let kind = self.ir.strings.resolve(ty.name);
        ty.open
            || ty
                .builtin
                .iter()
                .any(|member| self.ir.strings.resolve(*member).eq_ignore_ascii_case(name))
            || self.has_member(kind, name)
    }

    fn type_subtype_member(
        &self,
        type_id: rules::ir::TypeId,
        subtype: rules::ir::Symbol,
        name: &str,
    ) -> bool {
        self.has_subtype(
            self.ir.strings.resolve(self.ir.type_info(type_id).name),
            name,
            self.ir.strings.resolve(subtype),
        )
    }
}

pub fn parse_source(
    parser: &ParserKind,
    source: &str,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: &GameProfile,
) -> (Option<ParsedSource>, Option<Arc<HirFile>>) {
    parse_source_with_cache(
        parser,
        source,
        logical_path,
        rules,
        profile,
        None,
        None,
        None,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_source_with_cache(
    parser: &ParserKind,
    source: &str,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: &GameProfile,
    cache: Option<(&SourceFile, &ParseCache)>,
    ir: Option<&RulesIr>,
    facts: Option<&dyn SymbolFacts>,
    index_only: bool,
) -> (Option<ParsedSource>, Option<Arc<HirFile>>) {
    match parser {
        ParserKind::Script => {
            let parsed = cached_or_parse(FileFormat::Script, source, cache);
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| match ir {
                    Some(ir) if index_only => lower_shared_for_index_with_ir(
                        Arc::clone(&parsed),
                        path,
                        rules,
                        profile,
                        ir,
                        facts,
                    ),
                    Some(ir) => match facts {
                        Some(facts) => lower_shared_with_ir_and_facts(
                            Arc::clone(&parsed),
                            path,
                            rules,
                            profile,
                            ir,
                            facts,
                        ),
                        None => lower_shared_with_ir(Arc::clone(&parsed), path, rules, profile, ir),
                    },
                    None => lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
                },
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Localisation => {
            let parsed = cached_or_parse(FileFormat::Localisation, source, cache);
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| match ir {
                    Some(ir) if index_only => lower_shared_for_index_with_ir(
                        Arc::clone(&parsed),
                        path,
                        rules,
                        profile,
                        ir,
                        facts,
                    ),
                    Some(ir) => match facts {
                        Some(facts) => lower_shared_with_ir_and_facts(
                            Arc::clone(&parsed),
                            path,
                            rules,
                            profile,
                            ir,
                            facts,
                        ),
                        None => lower_shared_with_ir(Arc::clone(&parsed), path, rules, profile, ir),
                    },
                    None => lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
                },
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Asset | ParserKind::SyntaxOnly => (None, None),
    }
}

fn cached_or_parse(
    format: FileFormat,
    source: &str,
    cache: Option<(&SourceFile, &ParseCache)>,
) -> Arc<ParsedFile> {
    if let Some((file, cache)) = cache
        && let Some(parsed) = cache.load(file, format, source)
    {
        return Arc::new(parsed);
    }
    record_pipeline_parse();
    let parsed = Arc::new(parse(format, source));
    if let Some((file, cache)) = cache {
        // A cache write is an optimization only; a read-only or full disk cache must never make
        // the workspace scan fail after the source has already been parsed successfully.
        let _ = cache.store(file, format, source, &parsed);
    }
    parsed
}

fn parser_for_document(
    ir: Option<&RulesIr>,
    rules: &RuleSet,
    profile: &GameProfile,
    roots: &[SourceRoot],
    id: &DocumentId,
    path: Option<&Path>,
) -> Option<(ParserKind, Option<LogicalPath>)> {
    let logical = path
        .and_then(|path| {
            roots
                .iter()
                .filter_map(|root| path.strip_prefix(&root.path).ok())
                .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
                .min_by_key(|path| path.as_str().len())
        })
        .or_else(|| path.and_then(|path| rules.logical_path_for_uri(&path.to_string_lossy())))
        .or_else(|| rules.logical_path_for_uri(id.as_str()))
        .or_else(|| path.and_then(|path| LogicalPath::parse(&path.to_string_lossy()).ok()))
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    if logical
        .as_ref()
        .is_some_and(|path| profile.rejects_unlisted_root_file(path.as_str()))
    {
        return None;
    }
    if let Some(category) = logical
        .as_ref()
        .and_then(|path| ir.and_then(|ir| ir.file_rule(path).map(|(_, rule)| rule)))
    {
        return Some((parser_kind(category.parser), logical));
    }
    if ir.is_none_or(|ir| ir.files.is_empty())
        && let Some(category) = logical.as_ref().and_then(|path| rules.classify(path))
    {
        return Some((category.parser.clone(), logical));
    }
    let extension = path
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            logical.as_ref().and_then(|path| {
                path.as_str()
                    .rsplit_once('.')
                    .map(|(_, ext)| ext.to_ascii_lowercase())
            })
        })?;
    let parser = match extension.as_str() {
        "yml" | "yaml" => ParserKind::Localisation,
        "txt" | "gui" | "gfx" | "asset" | "sfx" => ParserKind::Script,
        _ => return None,
    };
    Some((parser, logical))
}

pub fn prepare_document_snapshot(
    rules: &RuleSet,
    profile: &GameProfile,
    roots: &[SourceRoot],
    document: DocumentSnapshot,
) -> DocumentSnapshot {
    prepare_document_snapshot_impl(rules, profile, None, None, roots, document)
}

/// Prepares a document using the active schema arena.
pub fn prepare_document_snapshot_with_ir(
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    roots: &[SourceRoot],
    document: DocumentSnapshot,
) -> DocumentSnapshot {
    prepare_document_snapshot_impl(rules, profile, Some(ir), None, roots, document)
}

/// Prepares a document using schema IR and candidate workspace symbol facts.
pub fn prepare_document_snapshot_with_ir_and_facts(
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    roots: &[SourceRoot],
    document: DocumentSnapshot,
) -> DocumentSnapshot {
    prepare_document_snapshot_impl(rules, profile, Some(ir), Some(facts), roots, document)
}

fn parser_kind(parser: DocumentParser) -> ParserKind {
    match parser {
        DocumentParser::Script => ParserKind::Script,
        DocumentParser::Localisation => ParserKind::Localisation,
        DocumentParser::Asset => ParserKind::Asset,
        DocumentParser::SyntaxOnly => ParserKind::SyntaxOnly,
    }
}

fn prepare_document_snapshot_impl(
    rules: &RuleSet,
    profile: &GameProfile,
    ir: Option<&RulesIr>,
    facts: Option<&dyn SymbolFacts>,
    roots: &[SourceRoot],
    mut document: DocumentSnapshot,
) -> DocumentSnapshot {
    let (parsed, hir) = parser_for_document(
        ir,
        rules,
        profile,
        roots,
        &document.id,
        document.path.as_deref(),
    )
    .map_or((None, None), |(parser, logical_path)| {
        parse_source_with_cache(
            &parser,
            &document.text,
            logical_path.as_ref(),
            rules,
            profile,
            None,
            ir,
            facts,
            false,
        )
    });
    document.parsed = parsed;
    document.hir = hir;
    document
}

pub fn unparsed_document(
    id: DocumentId,
    version: Option<i64>,
    text: String,
    source: DocumentSource,
    path: Option<AbsPath>,
) -> DocumentSnapshot {
    let line_index = LineIndex::new(&text);
    DocumentSnapshot {
        id,
        version,
        text: Arc::from(text),
        line_index,
        source,
        path,
        parsed: None,
        hir: None,
    }
}

pub fn staged_overlay_document(
    id: DocumentId,
    version: i64,
    text: String,
    path: Option<AbsPath>,
) -> DocumentSnapshot {
    unparsed_document(id, Some(version), text, DocumentSource::Overlay, path)
}

const MAX_SOURCE_WORKERS: usize = 12;
const PARALLEL_SOURCE_THRESHOLD: usize = 32;

pub struct SourceReadJob {
    pub file: SourceFile,
    pub physical_path: AbsPath,
    pub retain_frontend: bool,
}

pub struct SourceReadResult {
    file: SourceFile,
    state: Option<Arc<FileState>>,
    report: WorkspaceScanReport,
}

pub struct SourceLoadContext<'a> {
    pub limits: WorkspaceScanLimits,
    pub previous_files: &'a BTreeMap<SourceFileId, SourceFile>,
    pub previous_states: &'a BTreeMap<SourceFileId, Arc<FileState>>,
    pub rules: &'a RuleSet,
    pub profile: &'a GameProfile,
    pub ir: Option<&'a RulesIr>,
    pub parse_cache: Option<&'a ParseCache>,
    pub cancellation: &'a WorkspaceScanToken,
    pub progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

/// Relowers files that read workspace symbols against one immutable pass of facts.
/// Large files are lowered alone; small files use at most four frontends at once,
/// within the caller's worker limit. This bounds concurrent frontend allocations.
pub fn replay_symbol_dependent_files(
    files: &BTreeMap<SourceFileId, SourceFile>,
    states: &BTreeMap<SourceFileId, Arc<FileState>>,
    ir: &RulesIr,
    facts: &(dyn SymbolFacts + Sync),
    context: &SourceLoadContext<'_>,
) -> Result<BTreeMap<SourceFileId, Arc<FileState>>, WorkspaceError> {
    let mut jobs = states
        .iter()
        .filter(|(_, state)| state.symbol_facts_dependency)
        .collect::<Vec<_>>();
    // A large CST/HIR can be much larger than its source. Process those on the
    // calling thread before starting workers, so their allocation peaks cannot
    // overlap with other frontends or accumulate in separate worker arenas.
    jobs.sort_by_key(|(_, state)| std::cmp::Reverse(state.source().len()));
    const MAX_PARALLEL_SOURCE_BYTES: usize = 256 * 1024;
    let large_count =
        jobs.partition_point(|(_, state)| state.source().len() > MAX_PARALLEL_SOURCE_BYTES);
    let workers = thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(context.limits.max_workers.clamp(1, 4))
        .min(jobs.len());
    let rebuild = |id: &SourceFileId, state: &FileState| {
        context.cancellation.checkpoint()?;
        let Some(file) = files.get(id) else {
            return Ok(None);
        };
        let mut rebuilt = build_file_state_impl(
            file,
            state.source().to_owned(),
            state.revision(),
            context.rules,
            context.profile,
            context.parse_cache,
            Some(ir),
            Some(facts),
            state.parsed().is_none(),
        );
        if state.parsed().is_none() {
            rebuilt = rebuilt.cache_only();
        }
        Ok(Some((*id, Arc::new(rebuilt))))
    };
    let mut results = BTreeMap::new();
    for (id, state) in jobs.drain(..large_count) {
        if let Some((id, state)) = rebuild(id, state)? {
            results.insert(id, state);
        }
    }
    if jobs.len() < PARALLEL_SOURCE_THRESHOLD || workers < 2 {
        for (id, state) in jobs {
            if let Some((id, state)) = rebuild(id, state)? {
                results.insert(id, state);
            }
        }
        return Ok(results);
    }
    let queue = Mutex::new(VecDeque::from(jobs));
    thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut results = BTreeMap::new();
                    loop {
                        let job = queue
                            .lock()
                            .map_err(|_| {
                                WorkspaceError::Io(std::io::Error::other(
                                    "workspace replay queue was poisoned",
                                ))
                            })?
                            .pop_front();
                        let Some((id, state)) = job else { break };
                        if let Some((id, state)) = rebuild(id, state)? {
                            results.insert(id, state);
                        }
                    }
                    Ok::<_, WorkspaceError>(results)
                })
            })
            .collect::<Vec<_>>();
        let mut first_error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(batch)) => results.extend(batch),
                Ok(Err(error)) => {
                    first_error.get_or_insert(error);
                }
                Err(_) => {
                    first_error.get_or_insert_with(|| {
                        WorkspaceError::Io(std::io::Error::other(
                            "workspace replay worker panicked",
                        ))
                    });
                }
            }
        }
        first_error.map_or(Ok(results), Err)
    })
}

pub fn load_source_files(
    jobs: Vec<SourceReadJob>,
    files: &mut BTreeMap<SourceFileId, SourceFile>,
    file_states: &mut BTreeMap<SourceFileId, Arc<FileState>>,
    report: &mut WorkspaceScanReport,
    context: &SourceLoadContext<'_>,
) -> Result<(), WorkspaceError> {
    let total = jobs.len();
    let configured_workers = context.limits.max_workers.clamp(1, MAX_SOURCE_WORKERS);
    let worker_count = thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .min(MAX_SOURCE_WORKERS)
        .min(configured_workers)
        .min(jobs.len());
    let results = if jobs.len() < PARALLEL_SOURCE_THRESHOLD || worker_count < 2 {
        let mut results = Vec::with_capacity(jobs.len());
        let mut done = 0usize;
        for job in jobs {
            context.cancellation.checkpoint()?;
            results.push(load_source_file_job(job, context)?);
            done += 1;
            if let Some(progress) = context.progress {
                progress(done, total);
            }
        }
        results
    } else {
        let queue = Arc::new(Mutex::new(
            jobs.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut results = BTreeMap::new();
        thread::scope(|scope| -> Result<(), WorkspaceError> {
            let mut workers = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let queue = Arc::clone(&queue);
                let completed = Arc::clone(&completed);
                workers.push(scope.spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let job = match queue.lock() {
                            Ok(mut queue) => queue.pop_front(),
                            Err(_) => {
                                return Err(WorkspaceError::Io(std::io::Error::other(
                                    "workspace source worker queue was poisoned",
                                )));
                            }
                        };
                        let Some((index, job)) = job else {
                            break;
                        };
                        let result = load_source_file_job(job, context)?;
                        let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if let Some(progress) = context.progress {
                            progress(done, total);
                        }
                        results.push((index, result));
                    }
                    Ok(results)
                }));
            }
            let mut first_error = None;
            for worker in workers {
                match worker.join() {
                    Ok(Ok(worker_results)) => {
                        for (index, result) in worker_results {
                            results.insert(index, result);
                        }
                    }
                    Ok(Err(error)) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            first_error = Some(WorkspaceError::Io(std::io::Error::other(
                                "workspace source worker panicked",
                            )));
                        }
                    }
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            Ok(())
        })?;
        results.into_values().collect::<Vec<_>>()
    };

    for result in results {
        context.cancellation.checkpoint()?;
        merge_scan_report(report, result.report, context.limits);
        let Some(state) = result.state else {
            continue;
        };
        if let Some(existing) = files.insert(result.file.id, result.file.clone()) {
            return Err(WorkspaceError::FileIdCollision {
                first: existing.physical_path,
                second: result.file.physical_path,
            });
        }
        file_states.insert(result.file.id, state);
        report.indexed_files = report.indexed_files.saturating_add(1);
    }
    Ok(())
}

fn load_source_file_job(
    job: SourceReadJob,
    context: &SourceLoadContext<'_>,
) -> Result<SourceReadResult, WorkspaceError> {
    let mut report = WorkspaceScanReport::default();
    let text = read_source_file_cancellable(
        &job.physical_path,
        context.limits,
        &mut report,
        context.cancellation,
        context.profile.source_encoding,
    )?;
    let state = text.map(|text| {
        let previous = context.previous_states.get(&job.file.id);
        if let Some(previous) = previous
            && context.previous_files.get(&job.file.id) == Some(&job.file)
            && previous.source() == text
        {
            if job.retain_frontend || previous.parsed().is_none() {
                return Arc::clone(previous);
            }
            return Arc::new(previous.cache_only_from_existing());
        }
        let file_revision = previous.map_or(0, |state| state.revision().saturating_add(1));
        let state = build_file_state_impl(
            &job.file,
            text,
            file_revision,
            context.rules,
            context.profile,
            context.parse_cache,
            context.ir,
            None,
            !job.retain_frontend,
        );
        if job.retain_frontend {
            Arc::new(state)
        } else {
            Arc::new(state.cache_only())
        }
    });
    Ok(SourceReadResult {
        file: job.file,
        state,
        report,
    })
}

fn merge_scan_report(
    report: &mut WorkspaceScanReport,
    partial: WorkspaceScanReport,
    limits: WorkspaceScanLimits,
) {
    report.skipped_entries = report
        .skipped_entries
        .saturating_add(partial.skipped_entries);
    report.legacy_encoded_files = report
        .legacy_encoded_files
        .saturating_add(partial.legacy_encoded_files);
    report.omitted_issues = report.omitted_issues.saturating_add(partial.omitted_issues);
    for issue in partial.issues {
        if report.issues.len() < limits.max_reported_issues {
            report.issues.push(issue);
        } else {
            report.omitted_issues = report.omitted_issues.saturating_add(1);
        }
    }
}

pub fn build_file_state(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
) -> FileState {
    build_file_state_with_cache(file, source, revision, rules, profile, None)
}

pub fn build_file_state_with_cache(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
    parse_cache: Option<&ParseCache>,
) -> FileState {
    build_file_state_impl(
        file,
        source,
        revision,
        rules,
        profile,
        parse_cache,
        None,
        None,
        false,
    )
}

/// Materializes schema-driven HIR and its index shard.
pub fn build_file_state_with_ir(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    parse_cache: Option<&ParseCache>,
) -> FileState {
    build_file_state_impl(
        file,
        source,
        revision,
        rules,
        profile,
        parse_cache,
        Some(ir),
        None,
        false,
    )
}

/// Materializes an IR-driven file state using candidate workspace symbol facts.
#[allow(clippy::too_many_arguments)]
pub fn build_file_state_with_ir_and_facts(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    parse_cache: Option<&ParseCache>,
) -> FileState {
    build_file_state_impl(
        file,
        source,
        revision,
        rules,
        profile,
        parse_cache,
        Some(ir),
        Some(facts),
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_file_state_impl(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
    parse_cache: Option<&ParseCache>,
    ir: Option<&RulesIr>,
    facts: Option<&dyn SymbolFacts>,
    index_only: bool,
) -> FileState {
    let parser = if let Some(ir) = ir.filter(|ir| !ir.files.is_empty()) {
        ir.file_rule(&file.logical_path)
            .map(|(_, rule)| rule)
            .map(|category| parser_kind(category.parser))
    } else {
        rules
            .classify(&file.logical_path)
            .map(|category| category.parser.clone())
    };
    let Some(parser) = parser else {
        return FileState {
            revision,
            source: Arc::from(source),
            parsed: None,
            hir: None,
            symbol_facts_dependency: false,
            shard: Arc::new(FileIndexShard {
                file_id: file.id,
                definitions: Vec::new(),
                references: Vec::new(),
                dynamic_definitions: Vec::new(),
                definition_attributes: Vec::new(),
                flag_writes: Vec::new(),
                syntax_error_count: 0,
            }),
            cached_localisation_previews: None,
        };
    };
    let (parsed, hir) = parse_source_with_cache(
        &parser,
        &source,
        Some(&file.logical_path),
        rules,
        profile,
        parse_cache.map(|cache| (file, cache)),
        ir,
        facts,
        index_only,
    );
    let shard = match (parsed.as_ref(), hir.as_deref()) {
        (Some(ParsedSource::Text(parsed)), Some(hir)) => shard_for_source(file, parsed, hir, rules),
        (Some(ParsedSource::Text(parsed)), None) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            dynamic_definitions: Vec::new(),
            definition_attributes: Vec::new(),
            flag_writes: Vec::new(),
            syntax_error_count: parsed.errors().len(),
        },
        (None, _) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            dynamic_definitions: Vec::new(),
            definition_attributes: Vec::new(),
            flag_writes: Vec::new(),
            syntax_error_count: 0,
        },
    };
    let shared_source = match parsed.as_ref() {
        Some(ParsedSource::Text(parsed)) => parsed.source_handle(),
        None => Arc::from(source.as_str()),
    };
    FileState {
        revision,
        source: shared_source,
        parsed,
        symbol_facts_dependency: hir
            .as_ref()
            .is_some_and(|hir| hir.depends_on_symbol_facts()),
        hir,
        shard: Arc::new(shard),
        cached_localisation_previews: None,
    }
}

pub fn empty_file_state(file: &SourceFile, revision: u64) -> FileState {
    FileState {
        revision,
        source: Arc::from(""),
        parsed: None,
        hir: None,
        symbol_facts_dependency: false,
        shard: Arc::new(FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            dynamic_definitions: Vec::new(),
            definition_attributes: Vec::new(),
            flag_writes: Vec::new(),
            syntax_error_count: 0,
        }),
        cached_localisation_previews: None,
    }
}

pub fn position_ranges_for_state(state: &FileState) -> Vec<(TextRange, PositionRange)> {
    let line_index = LineIndex::new(state.source());
    state
        .shard()
        .definitions
        .iter()
        .filter_map(|definition| {
            line_index
                .position_range(state.source(), definition.selection_range)
                .map(|position| (definition.range, position))
        })
        .chain(state.shard().references.iter().filter_map(|reference| {
            line_index
                .position_range(state.source(), reference.range)
                .map(|position| (reference.range, position))
        }))
        .collect()
}

/// Builds the index shard for one parsed source, applying the same definition
/// deduplication the scan path uses. Public so the diagnostics-cache context
/// fingerprint can derive a shard from an overlay document's resident trees
/// through the identical code path — identical text therefore yields an
/// identical [`FileIndexShard::contribution_fingerprint`] whether the text
/// came from disk or from an overlay.
pub fn shard_for_source(
    file: &SourceFile,
    parsed: &ParsedFile,
    hir: &HirFile,
    rules: &RuleSet,
) -> FileIndexShard {
    let mut shard = shard_from_parsed(file, parsed, hir, rules);
    let mut seen_definitions = BTreeSet::new();
    shard.definitions.retain(|definition| {
        seen_definitions.insert((
            definition.kind.clone(),
            definition.name.clone(),
            definition.file_id,
            definition.range,
        ))
    });
    shard
}

fn shard_from_parsed(
    file: &SourceFile,
    parsed: &ParsedFile,
    hir: &HirFile,
    rules: &RuleSet,
) -> FileIndexShard {
    let mut definitions = Vec::new();
    let mut references = Vec::new();
    collect_hir_semantics(file, hir, &mut definitions, &mut references);
    // Shards stay resident in the workspace index; exact-fit the vectors so
    // growth doubling does not leave ~2x slack per file.
    definitions.shrink_to_fit();
    references.shrink_to_fit();
    let mut flag_writes = collect_flag_writes(hir, rules);
    flag_writes.shrink_to_fit();
    let dynamic_definitions = collect_dynamic_definitions(hir, rules);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        dynamic_definitions,
        definition_attributes: hir.definition_attributes().to_vec(),
        flag_writes,
        syntax_error_count: parsed.errors().len(),
    }
}

fn collect_flag_writes(hir: &HirFile, _rules: &RuleSet) -> Vec<crate::index::FlagWrite> {
    hir.definitions()
        .iter()
        .filter(|d| d.range == d.selection_range)
        .map(|d| crate::index::FlagWrite {
            kind: d.kind.clone(),
            name: vfs::intern_shard_string(&d.name),
            range: d.selection_range,
        })
        .collect()
}

fn collect_dynamic_definitions(hir: &HirFile, _rules: &RuleSet) -> Vec<DynamicDefinitionSummary> {
    let mut summaries = Vec::new();
    for definition in hir.definitions() {
        let enabled = hir
            .dynamic_template(&definition.kind, &definition.name, definition.range)
            .is_some();
        if !enabled {
            continue;
        }
        let parameters = hir
            .parameter_definitions_for_owner(definition.range)
            .map(|parameter| DynamicParameterSignature {
                name: parameter.name.clone(),
                required: hir.parameter_is_required(definition.range, &parameter.name),
            })
            .collect();
        summaries.push(DynamicDefinitionSummary {
            kind: definition.kind.clone(),
            name: definition.name.clone(),
            definition_range: definition.range,
            parameters,
            template: hir
                .dynamic_template(&definition.kind, &definition.name, definition.range)
                .cloned(),
        });
    }
    summaries.sort_by_key(|summary| summary.definition_range);
    summaries.dedup_by(|left, right| {
        left.definition_range == right.definition_range
            && left.kind.eq_ignore_ascii_case(&right.kind)
            && left.name.eq_ignore_ascii_case(&right.name)
    });
    summaries
}

fn collect_hir_semantics(
    file: &SourceFile,
    hir: &HirFile,
    definitions: &mut Vec<Definition>,
    references: &mut Vec<Reference>,
) {
    for definition in hir.definitions() {
        definitions.push(Definition {
            kind: vfs::intern_shard_string(&definition.kind),
            name: vfs::intern_shard_string(&definition.name),
            file_id: file.id,
            range: definition.range,
            selection_range: definition.selection_range,
            active: true,
        });
    }
    for reference in hir.references() {
        references.push(Reference {
            kind: vfs::intern_shard_string(&reference.kind),
            name: vfs::intern_shard_string(&reference.name),
            range: reference.range,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::{FileCategory, FileMatcher, FileResolutionPolicy};
    use std::path::PathBuf;
    use vfs::{SourceRootId, SourceRootKind, WorkspaceScanIssueKind};

    fn strict_common_profile() -> GameProfile {
        let mut profile = GameProfile::empty("test");
        profile.scan_roots = vec!["common".to_owned()];
        profile.scan_root_max_depths.insert("common".to_owned(), 0);
        profile
            .scan_root_files
            .insert("common".to_owned(), vec!["technology.txt".to_owned()]);
        profile
    }

    fn generic_script_rules() -> RuleSet {
        RuleSet::from_catalog(
            "test".to_owned(),
            vec![FileCategory {
                id: "script".to_owned(),
                parser: ParserKind::Script,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    path_exact: None,
                    extensions: vec!["txt".to_owned()],
                    path_suffix: None,
                    path_exclude_prefixes: Vec::new(),
                    case_sensitive: false,
                },
            }],
            GameProfile::empty("test"),
        )
    }

    #[test]
    fn strict_profile_prevents_generic_script_fallback_under_common() {
        let root = SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Project,
            AbsPath::normalize(&PathBuf::from("C:/fixture")),
        );
        let profile = strict_common_profile();
        let rules = generic_script_rules();
        let unknown = unparsed_document(
            DocumentId::new("file:///fixture/common/unknown.txt"),
            None,
            "unknown = yes".to_owned(),
            DocumentSource::Disk,
            Some(AbsPath::normalize(&root.path.join("common/unknown.txt"))),
        );
        let prepared = prepare_document_snapshot(&rules, &profile, &[root], unknown);
        assert!(prepared.parsed.is_none());
        assert!(prepared.hir.is_none());
    }

    #[test]
    fn strict_profile_keeps_an_exact_common_file_parseable() {
        let root = SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Project,
            AbsPath::normalize(&PathBuf::from("C:/fixture")),
        );
        let profile = strict_common_profile();
        let rules = generic_script_rules();
        let known = unparsed_document(
            DocumentId::new("file:///fixture/common/technology.txt"),
            None,
            "technology_group = { adm_tech = 1 }".to_owned(),
            DocumentSource::Disk,
            Some(AbsPath::normalize(&root.path.join("common/technology.txt"))),
        );
        let prepared = prepare_document_snapshot(&rules, &profile, &[root], known);
        assert!(prepared.parsed.is_some());
        assert!(prepared.hir.is_some());
    }

    fn fixture_source_file(id: SourceFileId, logical: &str) -> SourceFile {
        SourceFile {
            id,
            root_id: SourceRootId::new(3),
            physical_path: AbsPath::normalize(&PathBuf::from("C:/fixture").join(logical)),
            logical_path: LogicalPath::parse(logical).expect("logical path"),
            category_id: Some("script".to_owned()),
            resolution: FileResolutionPolicy::ReplaceByRelativePath,
        }
    }

    static NO_FILES: std::sync::LazyLock<BTreeMap<SourceFileId, SourceFile>> =
        std::sync::LazyLock::new(BTreeMap::new);
    static NO_STATES: std::sync::LazyLock<BTreeMap<SourceFileId, Arc<FileState>>> =
        std::sync::LazyLock::new(BTreeMap::new);

    fn load_context<'a>(
        rules: &'a RuleSet,
        profile: &'a GameProfile,
        cancellation: &'a WorkspaceScanToken,
    ) -> SourceLoadContext<'a> {
        SourceLoadContext {
            limits: WorkspaceScanLimits::default(),
            previous_files: &NO_FILES,
            previous_states: &NO_STATES,
            rules,
            profile,
            ir: None,
            parse_cache: None,
            cancellation,
            progress: None,
        }
    }

    #[test]
    fn document_classification_falls_back_to_uri_names_and_rejects_unknown_extensions() {
        let rules = generic_script_rules();
        let profile = GameProfile::empty("test");

        // Without a path, the URI-derived name still selects the script parser.
        let document = unparsed_document(
            DocumentId::new("file:///workspace/events/alpha.txt"),
            None,
            "key = yes\n".to_owned(),
            DocumentSource::Disk,
            None,
        );
        let prepared = prepare_document_snapshot(&rules, &profile, &[], document);
        assert!(prepared.parsed.is_some());
        assert!(prepared.hir.is_some());

        // A yml name selects the localisation frontend.
        let document = unparsed_document(
            DocumentId::new("file:///workspace/localisation/l_english.yml"),
            None,
            "l_english:\nkey:0 \"text\"\n".to_owned(),
            DocumentSource::Disk,
            None,
        );
        let prepared = prepare_document_snapshot(&rules, &profile, &[], document);
        let Some(ParsedSource::Text(parsed)) = &prepared.parsed else {
            panic!("yml document must parse");
        };
        assert_eq!(parsed.format(), FileFormat::Localisation);

        // Unknown extensions never reach a frontend.
        let document = unparsed_document(
            DocumentId::new("file:///workspace/assets/icon.png"),
            None,
            "binary".to_owned(),
            DocumentSource::Disk,
            None,
        );
        let prepared = prepare_document_snapshot(&rules, &profile, &[], document);
        assert!(prepared.parsed.is_none());
        assert!(prepared.hir.is_none());
    }

    #[test]
    fn malformed_scripts_still_shard_partial_definitions_and_count_syntax_errors() {
        let file_spec=serde_json::from_value(serde_json::json!({"types":{"event":{}},"files":{"script":{"path":"events","ext":"txt","root":"root"}},"schemas":{"root":{"fields":{"country_event":{"body":"event_body","card":"0..*","def":{"type":"event","name":"field:id"}}}},"event_body":{"fields":{"id":{"value":"scalar","card":"1"}}}}})).unwrap();
        let profile = GameProfile::empty("test");
        let ir = rules::lower::lower(
            &[("fixture.json".to_owned(), file_spec)],
            rules::ir::GameConfig {
                profile: profile.clone(),
            },
        )
        .unwrap();
        let rules = RuleSet::from_ir_catalog(&ir);
        let file = fixture_source_file(SourceFileId::new(11), "events/broken.txt");
        let state = build_file_state_with_ir(
            &file,
            "country_event = { id = broken.1 }\ntrailing = {\n".to_owned(),
            4,
            &rules,
            &profile,
            &ir,
            None,
        );
        assert!(state.parsed().is_some());
        // The intact event definition survives next to the unterminated block.
        assert_eq!(state.shard().syntax_error_count, 1);
        let definitions = state
            .shard()
            .definitions
            .iter()
            .map(|definition| (&*definition.kind, &*definition.name))
            .collect::<Vec<_>>();
        assert_eq!(definitions, [("event", "broken.1")]);
    }

    #[test]
    fn unclassified_paths_keep_an_empty_shard_and_the_raw_source() {
        let rules = generic_script_rules();
        let file = fixture_source_file(SourceFileId::new(12), "interface/unknown.gui");
        let state = build_file_state(
            &file,
            "unknown = yes\n".to_owned(),
            2,
            &rules,
            &GameProfile::empty("test"),
        );
        assert!(state.parsed().is_none());
        assert!(state.hir().is_none());
        assert_eq!(state.source(), "unknown = yes\n");
        assert!(state.shard().definitions.is_empty());
        assert!(state.shard().references.is_empty());
        assert_eq!(state.shard().syntax_error_count, 0);
    }

    #[test]
    fn load_source_files_reports_vanished_files_without_failing_the_scan() {
        let rules = generic_script_rules();
        let profile = GameProfile::empty("test");
        let token = WorkspaceScanToken::new();
        let context = load_context(&rules, &profile, &token);
        let job = SourceReadJob {
            file: fixture_source_file(SourceFileId::new(13), "events/vanished.txt"),
            physical_path: AbsPath::normalize(&PathBuf::from(
                "C:/fixture/does-not-exist/events/vanished.txt",
            )),
            retain_frontend: false,
        };

        let mut files = BTreeMap::new();
        let mut states = BTreeMap::new();
        let mut report = WorkspaceScanReport::default();
        load_source_files(vec![job], &mut files, &mut states, &mut report, &context)
            .expect("a vanished file is a reported issue, not a scan failure");
        assert!(files.is_empty());
        assert!(states.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert_eq!(
            report.issues[0].kind,
            WorkspaceScanIssueKind::FileUnreadable
        );
    }

    #[test]
    fn load_source_files_rejects_colliding_file_identities_and_cancellation() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("index-pipeline-collision-{nonce}"));
        std::fs::create_dir_all(root.join("events")).expect("events directory");
        std::fs::write(root.join("events/first.txt"), "key = 1\n").expect("first source");
        std::fs::write(root.join("events/second.txt"), "key = 2\n").expect("second source");

        let rules = generic_script_rules();
        let profile = GameProfile::empty("test");
        let token = WorkspaceScanToken::new();
        let context = load_context(&rules, &profile, &token);
        let shared_id = SourceFileId::new(14);
        let jobs = vec![
            SourceReadJob {
                file: fixture_source_file(shared_id, "events/first.txt"),
                physical_path: AbsPath::normalize(&root.join("events/first.txt")),
                retain_frontend: false,
            },
            SourceReadJob {
                file: SourceFile {
                    id: shared_id,
                    root_id: SourceRootId::new(3),
                    physical_path: AbsPath::normalize(&root.join("events/second.txt")),
                    logical_path: LogicalPath::parse("events/second.txt").expect("logical path"),
                    category_id: Some("script".to_owned()),
                    resolution: FileResolutionPolicy::ReplaceByRelativePath,
                },
                physical_path: AbsPath::normalize(&root.join("events/second.txt")),
                retain_frontend: false,
            },
        ];

        let mut files = BTreeMap::new();
        let mut states = BTreeMap::new();
        let error = load_source_files(
            jobs,
            &mut files,
            &mut states,
            &mut WorkspaceScanReport::default(),
            &context,
        )
        .expect_err("two physical files sharing one identity must fail");
        assert!(matches!(
            &error,
            WorkspaceError::FileIdCollision { first, second }
                if first.ends_with("events/first.txt") && second.ends_with("events/second.txt")
        ));

        let cancelled = WorkspaceScanToken::new();
        cancelled.cancel();
        let context = load_context(&rules, &profile, &cancelled);
        let error = load_source_files(
            vec![SourceReadJob {
                file: fixture_source_file(SourceFileId::new(15), "events/first.txt"),
                physical_path: AbsPath::normalize(&root.join("events/first.txt")),
                retain_frontend: false,
            }],
            &mut files,
            &mut states,
            &mut WorkspaceScanReport::default(),
            &context,
        )
        .expect_err("a cancelled token stops source loading");
        assert_eq!(error.to_string(), "workspace scan was cancelled");
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
