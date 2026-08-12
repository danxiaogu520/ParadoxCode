//! Parse, lower, and materialize immutable per-file pipeline state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile, parse};
use pdx_rules::{GameProfile, ParserKind, RuleSet};
use pdx_text::{LineIndex, LogicalPath, PositionRange, TextRange};

use crate::hir::{HirFile, lower_shared, lower_shared_with_profile};
use crate::index::{
    Definition, FileIndexShard, MacroDefinitionSummary, MacroParameterSignature, Reference,
};
use crate::model::{
    DocumentId, DocumentSnapshot, DocumentSource, FileState, ParsedSource, SourceFile,
    SourceFileId, SourceRoot, WorkspaceError, WorkspaceScanLimits, WorkspaceScanReport,
    WorkspaceScanToken,
};
use crate::scan::read_source_file_cancellable;
#[cfg(test)]
use crate::{record_pipeline_lower, record_pipeline_parse};

pub(crate) fn parse_source(
    parser: &ParserKind,
    source: &str,
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: &GameProfile,
) -> (Option<ParsedSource>, Option<Arc<HirFile>>) {
    match parser {
        ParserKind::Script => {
            #[cfg(test)]
            record_pipeline_parse();
            let parsed = Arc::new(parse(FileFormat::Script, source));
            #[cfg(test)]
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Localisation => {
            #[cfg(test)]
            record_pipeline_parse();
            let parsed = Arc::new(parse(FileFormat::Localisation, source));
            #[cfg(test)]
            record_pipeline_lower();
            let hir = Arc::new(logical_path.map_or_else(
                || lower_shared(Arc::clone(&parsed), rules),
                |path| lower_shared_with_profile(Arc::clone(&parsed), path, rules, profile),
            ));
            (Some(ParsedSource::Text(parsed)), Some(hir))
        }
        ParserKind::Asset | ParserKind::SyntaxOnly => (None, None),
    }
}

fn parser_for_document(
    rules: &RuleSet,
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
        .or_else(|| path.and_then(|path| LogicalPath::parse(&path.to_string_lossy()).ok()))
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    if let Some(category) = logical.as_ref().and_then(|path| rules.classify(path)) {
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

pub(crate) fn prepare_document_snapshot(
    rules: &RuleSet,
    profile: &GameProfile,
    roots: &[SourceRoot],
    mut document: DocumentSnapshot,
) -> DocumentSnapshot {
    let (parsed, hir) = parser_for_document(rules, roots, &document.id, document.path.as_deref())
        .map_or((None, None), |(parser, logical_path)| {
            parse_source(
                &parser,
                &document.text,
                logical_path.as_ref(),
                rules,
                profile,
            )
        });
    document.parsed = parsed;
    document.hir = hir;
    document
}

pub(crate) fn unparsed_document(
    id: DocumentId,
    version: Option<i64>,
    text: String,
    source: DocumentSource,
    path: Option<PathBuf>,
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

pub(crate) fn staged_overlay_document(
    id: DocumentId,
    version: i64,
    text: String,
    path: Option<PathBuf>,
) -> DocumentSnapshot {
    unparsed_document(id, Some(version), text, DocumentSource::Overlay, path)
}

const MAX_SOURCE_WORKERS: usize = 12;
const PARALLEL_SOURCE_THRESHOLD: usize = 32;

pub(crate) struct SourceReadJob {
    pub(crate) file: SourceFile,
    pub(crate) physical_path: PathBuf,
    pub(crate) retain_frontend: bool,
}

pub(crate) struct SourceReadResult {
    file: SourceFile,
    state: Option<Arc<FileState>>,
    report: WorkspaceScanReport,
}

pub(crate) struct SourceLoadContext<'a> {
    pub(crate) limits: WorkspaceScanLimits,
    pub(crate) previous_files: &'a BTreeMap<SourceFileId, SourceFile>,
    pub(crate) previous_states: &'a BTreeMap<SourceFileId, Arc<FileState>>,
    pub(crate) rules: &'a RuleSet,
    pub(crate) profile: &'a GameProfile,
    pub(crate) cancellation: &'a WorkspaceScanToken,
    pub(crate) progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

pub(crate) fn load_source_files(
    jobs: Vec<SourceReadJob>,
    files: &mut BTreeMap<SourceFileId, SourceFile>,
    file_states: &mut BTreeMap<SourceFileId, Arc<FileState>>,
    report: &mut WorkspaceScanReport,
    context: &SourceLoadContext<'_>,
) -> Result<(), WorkspaceError> {
    let total = jobs.len();
    let worker_count = thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .min(MAX_SOURCE_WORKERS)
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
            return Arc::new(
                previous.cache_only_from_existing(position_ranges_for_state(previous)),
            );
        }
        let file_revision = previous.map_or(0, |state| state.revision().saturating_add(1));
        let state = build_file_state(
            &job.file,
            text,
            file_revision,
            context.rules,
            context.profile,
        );
        if job.retain_frontend {
            Arc::new(state)
        } else {
            let positions = position_ranges_for_state(&state);
            Arc::new(state.cache_only(positions))
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

pub(crate) fn build_file_state(
    file: &SourceFile,
    source: String,
    revision: u64,
    rules: &RuleSet,
    profile: &GameProfile,
) -> FileState {
    let Some(category) = rules.classify(&file.logical_path) else {
        return FileState {
            revision,
            source: Arc::from(source),
            parsed: None,
            hir: None,
            shard: Arc::new(FileIndexShard {
                file_id: file.id,
                definitions: Vec::new(),
                references: Vec::new(),
                macro_definitions: Vec::new(),
                syntax_error_count: 0,
            }),
            cached_positions: None,
            cached_localisation_previews: None,
        };
    };
    let (parsed, hir) = parse_source(
        &category.parser,
        &source,
        Some(&file.logical_path),
        rules,
        profile,
    );
    let mut shard = match (parsed.as_ref(), hir.as_deref()) {
        (Some(ParsedSource::Text(parsed)), Some(hir)) => {
            shard_from_parsed(file, parsed, hir, rules)
        }
        (Some(ParsedSource::Text(parsed)), None) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: parsed.errors().len(),
        },
        (None, _) => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        },
    };
    let mut seen_definitions = BTreeSet::new();
    shard.definitions.retain(|definition| {
        seen_definitions.insert((
            definition.kind.clone(),
            definition.name.clone(),
            definition.file_id,
            definition.range,
        ))
    });
    FileState {
        revision,
        source: Arc::from(source),
        parsed,
        hir,
        shard: Arc::new(shard),
        cached_positions: None,
        cached_localisation_previews: None,
    }
}

pub(crate) fn empty_file_state(file: &SourceFile, revision: u64) -> FileState {
    FileState {
        revision,
        source: Arc::from(""),
        parsed: None,
        hir: None,
        shard: Arc::new(FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            macro_definitions: Vec::new(),
            syntax_error_count: 0,
        }),
        cached_positions: None,
        cached_localisation_previews: None,
    }
}

pub(crate) fn position_ranges_for_state(state: &FileState) -> Vec<(TextRange, PositionRange)> {
    if let Some(cached) = state.cached_positions.as_deref() {
        return cached.clone();
    }
    let line_index = LineIndex::new(state.source());
    let hir_selection_ranges = state
        .hir()
        .map(|hir| {
            hir.definitions()
                .iter()
                .map(|definition| {
                    (
                        (
                            definition.kind.clone(),
                            definition.name.clone(),
                            definition.range,
                        ),
                        definition.selection_range,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    state
        .shard()
        .definitions
        .iter()
        .filter_map(|definition| {
            let selection_range = hir_selection_ranges
                .get(&(
                    definition.kind.clone(),
                    definition.name.clone(),
                    definition.range,
                ))
                .copied()
                .unwrap_or(definition.range);
            line_index
                .position_range(state.source(), selection_range)
                .map(|position| (definition.range, position))
        })
        .chain(state.shard().references.iter().filter_map(|reference| {
            line_index
                .position_range(state.source(), reference.range)
                .map(|position| (reference.range, position))
        }))
        .collect()
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
    collect_semantic_type_members(file, parsed, rules, &mut definitions);
    let macro_definitions = collect_macro_definitions(hir, rules);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        macro_definitions,
        syntax_error_count: parsed.errors().len(),
    }
}

fn collect_macro_definitions(hir: &HirFile, rules: &RuleSet) -> Vec<MacroDefinitionSummary> {
    let mut summaries = Vec::new();
    for definition in hir.definitions() {
        let macro_enabled = rules
            .model()
            .semantic
            .type_descriptors
            .iter()
            .find(|(kind, _)| kind.eq_ignore_ascii_case(&definition.kind))
            .and_then(|(_, descriptor)| descriptor.scripted_macro.as_ref())
            .is_some_and(|descriptor| descriptor.macro_enabled);
        if !macro_enabled {
            continue;
        }
        let parameters = hir
            .parameter_definitions_for_owner(definition.range)
            .map(|parameter| MacroParameterSignature {
                name: parameter.name.clone(),
                required: hir.parameter_is_required(definition.range, &parameter.name),
            })
            .collect();
        summaries.push(MacroDefinitionSummary {
            kind: definition.kind.clone(),
            name: definition.name.clone(),
            definition_range: definition.range,
            parameters,
            template: hir
                .macro_template(&definition.kind, &definition.name, definition.range)
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

/// Collects workspace members declared by semantic `type[...]` definitions.
///
/// The semantic engine builds these members from the parsed workspace rather than treating a type's name as
/// a literal root key. For example, `type[mission]` with `skip_root_key = any` exposes every child
/// of every root clause in `missions/*.txt` as a `<mission>` member. Keeping this in the workspace
/// shard makes semantic key/value matching, completion, and hover see the same dynamic names.
fn collect_semantic_type_members(
    file: &SourceFile,
    parsed: &ParsedFile,
    rules: &RuleSet,
    definitions: &mut Vec<Definition>,
) {
    for descriptor in rules.model().semantic.type_descriptors.values() {
        if !semantic_type_path_matches(descriptor, &file.logical_path) {
            continue;
        }
        // File-based type instances are emitted by HIR with the filename range. The generic
        // property collector must not reinterpret their top-level fields as type members.
        if descriptor.type_per_file {
            continue;
        }

        if descriptor.skip_root_paths.is_empty() {
            for child in parsed.root().children() {
                if child.kind() == CstKind::Property {
                    collect_semantic_type_definition(file, parsed, descriptor, child, definitions);
                }
            }
        } else {
            for root in parsed.root().children() {
                if root.kind() != CstKind::Property {
                    continue;
                }
                for skip_path in &descriptor.skip_root_paths {
                    collect_semantic_skip_root_path(
                        file,
                        parsed,
                        descriptor,
                        root,
                        skip_path,
                        definitions,
                    );
                }
            }
        }
    }
}

fn collect_semantic_skip_root_path(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    path: &[String],
    definitions: &mut Vec<Definition>,
) {
    let Some(head) = path.first() else {
        collect_semantic_block_children(file, parsed, descriptor, node, definitions);
        return;
    };
    let node_key = semantic_property_key(node, parsed).unwrap_or_default();
    if !head.eq_ignore_ascii_case("any") && !head.eq_ignore_ascii_case(&node_key) {
        return;
    }
    if path.len() == 1 {
        collect_semantic_block_children(file, parsed, descriptor, node, definitions);
        return;
    }
    for child in semantic_block_properties(node) {
        collect_semantic_skip_root_path(file, parsed, descriptor, child, &path[1..], definitions);
    }
}

fn collect_semantic_block_children(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    for child in semantic_block_properties(node) {
        collect_semantic_type_definition(file, parsed, descriptor, child, definitions);
    }
}

fn collect_semantic_type_definition(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_rules::TypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    let Some(key) = semantic_property_key(node, parsed) else {
        return;
    };
    if !semantic_type_key_matches(descriptor, &key) {
        return;
    }
    let Some(name) = descriptor
        .name_field
        .as_deref()
        .and_then(|field| find_property(node, field, parsed))
        .or(Some(key))
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    definitions.push(Definition {
        kind: descriptor.name.clone(),
        name,
        file_id: file.id,
        range: node.range(),
        active: true,
    });
}

fn semantic_type_key_matches(descriptor: &pdx_rules::TypeDescriptor, key: &str) -> bool {
    descriptor
        .type_key_filter
        .as_ref()
        .is_none_or(|(values, negate)| {
            (values.iter().any(|value| value.eq_ignore_ascii_case(key))) != *negate
        })
}

fn semantic_block_properties(node: &CstNode) -> impl Iterator<Item = &CstNode> {
    node.children().iter().flat_map(|child| {
        if child.kind() != CstKind::Value {
            return Vec::new();
        }
        child
            .children()
            .iter()
            .filter(|block| block.kind() == CstKind::Block)
            .flat_map(|block| {
                block
                    .children()
                    .iter()
                    .filter(|child| child.kind() == CstKind::Property)
            })
            .collect::<Vec<_>>()
    })
}

fn semantic_property_key(node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    node.children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)
        .and_then(|child| parsed.text(child.range()))
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn semantic_type_path_matches(
    descriptor: &pdx_rules::TypeDescriptor,
    logical_path: &LogicalPath,
) -> bool {
    let path = logical_path
        .as_str()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let (directory, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix = prefix
            .trim_matches('/')
            .strip_prefix("game/")
            .unwrap_or(prefix.trim_matches('/'));
        let prefix = prefix.to_ascii_lowercase();
        let matches = if descriptor.path_strict {
            directory == prefix
        } else {
            directory == prefix || directory.starts_with(&format!("{prefix}/"))
        };
        if !matches {
            return false;
        }
    }
    if let Some(expected_file) = descriptor.path_file.as_deref()
        && !file_name.eq_ignore_ascii_case(expected_file)
    {
        return false;
    }
    if let Some(expected_extension) = descriptor.path_extension.as_deref() {
        let expected_extension = expected_extension.trim_start_matches('.');
        let actual_extension = file_name
            .rsplit_once('.')
            .map_or("", |(_, extension)| extension);
        if !actual_extension.eq_ignore_ascii_case(expected_extension) {
            return false;
        }
    }
    true
}

fn collect_hir_semantics(
    file: &SourceFile,
    hir: &HirFile,
    definitions: &mut Vec<Definition>,
    references: &mut Vec<Reference>,
) {
    for definition in hir.definitions() {
        definitions.push(Definition {
            kind: definition.kind.clone(),
            name: definition.name.clone(),
            file_id: file.id,
            range: definition.range,
            active: true,
        });
    }
    for reference in hir.references() {
        references.push(Reference {
            kind: reference.kind.clone(),
            name: reference.name.clone(),
            file_id: file.id,
            range: reference.range,
        });
    }
}

fn find_property(node: &CstNode, wanted: &str, parsed: &ParsedFile) -> Option<String> {
    if node.kind() == CstKind::Property {
        let key = node
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Key)
            .and_then(|child| parsed.text(child.range()))
            .map(str::trim);
        if key == Some(wanted) {
            for child in node.children() {
                if matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString) {
                    return parsed
                        .text(child.range())
                        .map(|value| value.trim_matches('"').trim().to_owned());
                }
                if child.kind() == CstKind::Value
                    && let Some(value) = child.children().iter().find(|value| {
                        matches!(value.kind(), CstKind::BareValue | CstKind::QuotedString)
                    })
                {
                    return parsed
                        .text(value.range())
                        .map(|value| value.trim_matches('"').trim().to_owned());
                }
            }
        }
    }
    node.children()
        .iter()
        .find_map(|child| find_property(child, wanted, parsed))
}
