use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use engine::{AnalysisSnapshot, Definition, DocumentId, DocumentSource, Reference, SourceFileId};
use hir::{HirFile, HirReference};
#[cfg(test)]
use std::cell::Cell;
use text::{LogicalPath, TextRange, TextSize};

use crate::semantic::*;
use crate::support::*;
use crate::types::*;

#[derive(Clone, Debug)]
pub(crate) struct DefinitionInfo {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) symbol: Symbol,
    pub(crate) document: Option<DocumentId>,
    pub(crate) file: Option<SourceFileId>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReferenceInternal {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) range: TextRange,
    pub(crate) document: Option<DocumentId>,
    pub(crate) file: Option<SourceFileId>,
    pub(crate) path: Option<LogicalPath>,
}

impl ReferenceInternal {
    pub(crate) fn location(&self) -> Location {
        Location {
            document: self.document.clone(),
            file: self.file,
            path: self.path.clone(),
            range: self.range,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SemanticWorkspace {
    pub(crate) coverage: hir::analysis::AnalysisCoverage,
    pub(crate) blocked_edits: BTreeSet<(String, String)>,
    pub(crate) definitions: Vec<DefinitionInfo>,
    pub(crate) references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SemanticFile {
    pub(crate) coverage: hir::analysis::AnalysisCoverage,
    pub(crate) blocked_edits: BTreeSet<(String, String)>,
    pub(crate) definitions: Vec<DefinitionInfo>,
    pub(crate) references: Vec<ReferenceInternal>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolutionDefinition {
    pub(crate) location: Location,
    pub(crate) selection_range: TextRange,
    pub(crate) priority: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RenameTarget {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) cursor_range: TextRange,
    pub(crate) definition: ResolutionDefinition,
}

pub(crate) enum Resolution {
    Unique(ResolutionDefinition),
    Missing,
}

pub(crate) fn semantic_data(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> SemanticFile {
    uncancelled(semantic_data_with_cancellation(
        snapshot,
        input,
        &CancellationToken::new(),
    ))
}

pub(crate) fn semantic_data_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<SemanticFile, Cancelled> {
    cancellation.checkpoint()?;
    // Overlay documents are re-extracted by every query (diagnostics, hover, completion,
    // navigation). The result only depends on the immutable snapshot, so cache it per
    // (revision, document) and share it across all worker threads observing that revision.
    let Some(document) = input.document.as_ref() else {
        return semantic_data_with_cancellation_uncached(snapshot, input, cancellation);
    };
    let revision = snapshot.revision();
    let key = document.as_str();
    if let Some(cached) = snapshot.query_cache().get::<SemanticFile>(revision, key) {
        return Ok((*cached).clone());
    }
    let data = semantic_data_with_cancellation_uncached(snapshot, input, cancellation)?;
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        key.to_owned(),
        Arc::new(data.clone()),
    );
    Ok(data)
}

fn semantic_data_with_cancellation_uncached(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<SemanticFile, Cancelled> {
    let mut data = SemanticFile::default();
    let Some(hir) = input.hir.as_deref() else {
        collect_template_semantics(snapshot, input, &mut data, cancellation)?;
        return Ok(data);
    };

    data.coverage.merge(hir.analysis_coverage());
    for definition in hir.definitions() {
        cancellation.checkpoint()?;
        data.definitions.push(make_definition(
            input,
            &definition.kind,
            definition.name.clone(),
            definition.range,
            definition.selection_range,
        ));
    }
    for reference in hir.references() {
        cancellation.checkpoint()?;
        if !ir_reference_is_template(snapshot, hir, reference) {
            continue;
        }
        data.references.push(ReferenceInternal {
            kind: reference.kind.to_string(),
            name: reference.name.clone(),
            range: reference.range,
            document: input.document.clone(),
            file: input.file,
            path: input.path.clone(),
        });
    }
    collect_template_semantics(snapshot, input, &mut data, cancellation)?;
    Ok(data)
}

fn collect_template_semantics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    data: &mut SemanticFile,
    cancellation: &CancellationToken,
) -> Result<(), Cancelled> {
    if input.format != parser::FileFormat::Script {
        return Ok(());
    }

    crate::ir_template::for_each_consumption(
        snapshot,
        input,
        cancellation,
        &mut |source, invocation, body| {
            data.coverage.merge(&body.coverage);
            for call in &body.rendered.calls {
                let mapped = call.source.as_ref().and_then(|(parameter, range)| {
                    hir::template_instance::project_binding_range(
                        parameter,
                        *range,
                        source.syntax(),
                        source.properties_in_range(invocation.range).filter(|arg| {
                            arg.path.len() == invocation.path.len() + 1
                                && arg.path.starts_with(&invocation.path)
                        }),
                    )
                });
                if let Some(range) = mapped {
                    data.references.push(ReferenceInternal {
                        kind: call.kind.clone(),
                        name: call.name.clone(),
                        range,
                        document: input.document.clone(),
                        file: input.file,
                        path: input.path.clone(),
                    });
                } else {
                    data.blocked_edits.insert((
                        call.kind.to_ascii_lowercase(),
                        call.name.to_ascii_lowercase(),
                    ));
                }
            }
            for definition in body.hir.definitions() {
                cancellation.checkpoint()?;
                if body
                    .rendered
                    .dependencies(definition.selection_range)
                    .is_empty()
                {
                    continue;
                }
                let Some(selection) = crate::ir_template::project_source_range(
                    body,
                    definition.selection_range,
                    source,
                    invocation,
                ) else {
                    data.blocked_edits.insert((
                        definition.kind.to_ascii_lowercase(),
                        definition.name.to_ascii_lowercase(),
                    ));
                    continue;
                };
                let range = crate::ir_template::project_source_range(
                    body,
                    definition.range,
                    source,
                    invocation,
                )
                .unwrap_or(selection);
                data.definitions.push(make_definition(
                    input,
                    &definition.kind,
                    definition.name.clone(),
                    range,
                    selection,
                ));
            }
            for reference in body.hir.references() {
                cancellation.checkpoint()?;
                if body.rendered.dependencies(reference.range).is_empty() {
                    continue;
                }
                let Some(range) = crate::ir_template::project_source_range(
                    body,
                    reference.range,
                    source,
                    invocation,
                ) else {
                    data.blocked_edits.insert((
                        reference.kind.to_ascii_lowercase(),
                        reference.name.to_ascii_lowercase(),
                    ));
                    continue;
                };
                data.references.push(ReferenceInternal {
                    kind: reference.kind.to_string(),
                    name: reference.name.clone(),
                    range,
                    document: input.document.clone(),
                    file: input.file,
                    path: input.path.clone(),
                });
            }
            Ok(())
        },
    )?;
    data.references.sort_by(|left, right| {
        (&left.kind, &left.name, left.range).cmp(&(&right.kind, &right.name, right.range))
    });
    data.references.dedup_by(|left, right| {
        left.kind == right.kind && left.name == right.name && left.range == right.range
    });
    data.definitions.sort_by(|left, right| {
        (&left.kind, &left.name, left.symbol.selection_range).cmp(&(
            &right.kind,
            &right.name,
            right.symbol.selection_range,
        ))
    });
    data.definitions.dedup_by(|left, right| {
        left.kind == right.kind
            && left.name == right.name
            && left.symbol.selection_range == right.symbol.selection_range
    });
    Ok(())
}

/// HIR records the declared key reference even when the invocation value is
/// malformed. Navigation must apply the same scalar form and required-argument
/// checks as diagnostics, using the actual compiled field rather than a legacy
/// type descriptor.
fn ir_reference_is_template(
    snapshot: &AnalysisSnapshot,
    hir: &HirFile,
    reference: &HirReference,
) -> bool {
    let ir = snapshot.ir();
    let Some(property) = hir.property_at_key_range(reference.range) else {
        return true;
    };
    let Some(fact) = hir.field_fact_at(property.key_range) else {
        return true;
    };
    let fields = fact
        .fields
        .iter()
        .filter(|id| {
            crate::ir_template::template_kind(ir, ir.field(**id).key)
                .is_some_and(|kind| kind.eq_ignore_ascii_case(&reference.kind))
        })
        .collect::<Vec<_>>();
    if fields.is_empty() || property.scalar.is_none() {
        return true;
    }
    let Some(summary) = dynamic_definition_summary(snapshot, &reference.kind, &reference.name)
    else {
        return false;
    };
    if summary.parameters.iter().any(|parameter| {
        crate::dynamic_rules::parameter_effectively_required(snapshot, &summary, parameter)
    }) {
        return false;
    }
    let scalar = property.scalar.as_ref().unwrap();
    let state = crate::ir_template::invocation_state(hir, property);
    fields.into_iter().any(|id| match ir.field(*id).value {
        rules::ir::FieldValue::Scalar(matcher) => crate::ir_semantic::matcher_matches_with_state(
            snapshot,
            ir,
            matcher,
            &scalar.value,
            &crate::ir_semantic::WorkspaceFacts { snapshot },
            &state,
        ),
        _ => false,
    })
}

pub(crate) fn text_range_within(inner: TextRange, outer: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}
pub(crate) fn make_definition(
    input: &ParsedInput,
    kind: &str,
    name: String,
    range: TextRange,
    selection_range: TextRange,
) -> DefinitionInfo {
    let location = Location {
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        range,
    };
    DefinitionInfo {
        kind: kind.to_owned(),
        name: name.clone(),
        symbol: Symbol {
            name,
            kind: kind.to_owned(),
            range,
            selection_range,
            location,
        },
        document: input.document.clone(),
        file: input.file,
    }
}
pub(crate) fn all_semantics(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<SemanticWorkspace, Cancelled> {
    all_semantics_inner(snapshot, cancellation, &mut |_file, _state| true)
}

/// Collects quoted semantics only from files whose retained source text
/// mentions any of `names` (ASCII case-insensitively), plus every open
/// overlay document.
///
/// Reference and rename results can only mention files that contain the
/// symbol name literally — every semantic name is derived from source tokens —
/// so a substring test is a safe superset filter. It survives syntax-tree
/// eviction (the source text stays resident) and avoids reparsing files that
/// cannot contribute, keeping the query bounded on large mods.
pub(crate) fn all_semantics_for_symbol(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
    names: &[&str],
) -> Result<SemanticWorkspace, Cancelled> {
    #[cfg(test)]
    ALL_SEMANTICS_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    let needles = names
        .iter()
        .map(|name| (*name).to_ascii_lowercase())
        .collect::<Vec<_>>();
    all_semantics_inner(snapshot, cancellation, &mut |_file, state| {
        if state.symbol_facts_dependency {
            return true;
        }
        let source = state.source().as_bytes();
        needles.iter().any(|needle| {
            source
                .windows(needle.len().max(1))
                .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
        })
    })
}

fn all_semantics_inner(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
    file_filter: &mut dyn FnMut(&engine::SourceFile, &engine::FileState) -> bool,
) -> Result<SemanticWorkspace, Cancelled> {
    #[cfg(test)]
    ALL_SEMANTICS_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    let mut all = SemanticWorkspace::default();
    // Indexed definitions and references remain in `AnalysisSnapshot::index()` and are consulted
    // by targeted candidate/reference iterators below. Keeping them out of this temporary
    // semantic workspace avoids cloning every cached Vanilla symbol for each query.
    let overlay_files = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect::<BTreeSet<_>>();
    for file in snapshot.source_files().values() {
        cancellation.checkpoint()?;
        if overlay_files.contains(&file.id) {
            continue;
        }
        if snapshot
            .index()
            .shard(file.id)
            .is_some_and(|shard| !shard.reference_coverage_known)
        {
            all.coverage
                .limits
                .insert(hir::analysis::AnalysisLimit::DependentQuery);
        }
        let Some(state) = snapshot.file_state(file.id) else {
            continue;
        };
        if !file_filter(file, state) {
            continue;
        }
        let Some(input) = input_for_source_file(snapshot, file.id) else {
            continue;
        };
        let mut quoted = SemanticFile::default();
        collect_template_semantics(snapshot, &input, &mut quoted, cancellation)?;
        all.coverage.merge(&quoted.coverage);
        all.blocked_edits.extend(quoted.blocked_edits);
        all.definitions.extend(quoted.definitions);
        all.references.extend(quoted.references);
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            let semantic = semantic_data_with_cancellation(snapshot, &input, cancellation)?;
            all.coverage.merge(&semantic.coverage);
            all.blocked_edits.extend(semantic.blocked_edits);
            all.definitions.extend(semantic.definitions);
            all.references.extend(semantic.references);
        }
    }
    Ok(all)
}

#[cfg(test)]
thread_local! {
    pub(crate) static ALL_SEMANTICS_CALLS: Cell<usize> = const { Cell::new(0) };
}
pub(crate) fn resolve_symbol(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Resolution {
    let candidates = symbol_candidates(snapshot, all, kind, name);
    if candidates.is_empty() {
        return Resolution::Missing;
    }
    let mut ordered = retain_highest_and_order(candidates);
    match ordered.pop() {
        Some(winner) => Resolution::Unique(winner),
        None => Resolution::Missing,
    }
}

pub(crate) fn symbol_candidates(
    snapshot: &AnalysisSnapshot,
    all: &SemanticWorkspace,
    kind: &str,
    name: &str,
) -> Vec<ResolutionDefinition> {
    let overlay_files = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect::<BTreeSet<_>>();
    let mut candidates = all
        .definitions
        .iter()
        .filter(|definition| definition.kind == kind && same_name(&definition.name, name))
        .filter(|definition| {
            definition
                .file
                .is_none_or(|file| !overlay_files.contains(&file) || definition.document.is_some())
        })
        .map(|definition| ResolutionDefinition {
            location: definition.symbol.location.clone(),
            selection_range: definition.symbol.selection_range,
            priority: definition_priority(snapshot, definition),
        })
        .collect::<Vec<_>>();
    // Indexed definitions are the normal source of Vanilla/dependency candidates. Add them even
    // when an overlay supplied a same-named semantic definition: merge/unique policies need to
    // see every candidate, while overlay files hide their corresponding disk entries.
    for definition in snapshot.index().definitions(kind, name) {
        if overlay_files.contains(&definition.file_id) {
            continue;
        }
        candidates.push(index_definition(snapshot, definition));
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    if kind.eq_ignore_ascii_case("localisation") {
        candidates = prefer_localisation_language_for_snapshot(snapshot, candidates);
    }
    candidates
}

pub(crate) fn symbol_candidates_for_hover(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<ResolutionDefinition>, Cancelled> {
    let overlay_files = overlay_file_ids(snapshot);
    let mut candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        cancellation.checkpoint()?;
        let Some(input) = input_for_document(snapshot, document.id()) else {
            continue;
        };
        for definition in semantic_data(snapshot, &input).definitions {
            cancellation.checkpoint()?;
            if definition.kind != kind || !same_name(&definition.name, name) {
                continue;
            }
            let priority = definition_priority(snapshot, &definition);
            candidates.push(ResolutionDefinition {
                location: definition.symbol.location,
                selection_range: definition.symbol.selection_range,
                priority,
            });
        }
    }
    for definition in snapshot.index().definitions(kind, name) {
        cancellation.checkpoint()?;
        if overlay_files.contains(&definition.file_id) {
            continue;
        }
        candidates.push(index_definition(snapshot, definition));
    }
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| {
            symbol_location_sort_key(&left.location).cmp(&symbol_location_sort_key(&right.location))
        })
    });
    candidates.dedup_by(|left, right| {
        left.location == right.location && left.selection_range == right.selection_range
    });
    if kind.eq_ignore_ascii_case("localisation") {
        candidates = prefer_localisation_language_for_snapshot(snapshot, candidates);
    }
    Ok(candidates)
}

pub(crate) fn symbol_location_sort_key(location: &Location) -> (String, u32, u32) {
    (
        location
            .path
            .as_ref()
            .map_or_else(String::new, |path| path.as_str().to_owned()),
        location.range.start(),
        location.range.end(),
    )
}

/// Extracts the localisation language from a file's logical path. Both `localisation/l_english/...`
/// and `localisation/domination_l_english.yml` yield `english`; non-localisation files yield `None`.
pub(crate) fn localisation_language(path: Option<&LogicalPath>) -> Option<String> {
    let path = path?.as_str();
    for segment in path.split('/') {
        if let Some(language) = segment
            .strip_prefix("l_")
            .filter(|language| !language.is_empty())
        {
            let language = language
                .strip_suffix(".yml")
                .or_else(|| language.strip_suffix(".yaml"))
                .unwrap_or(language);
            return Some(language.to_owned());
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
    (!language.is_empty()).then(|| language.to_owned())
}

/// Orders localisation candidates for navigation: the workspace target
/// language (the first configured preference, English by default) first, with
/// English as the fixed fallback so keys defined only in other languages stay
/// navigable under any configured target. Candidates are reordered, never
/// excluded — a set matching no listed language passes through unchanged so
/// single-language mods still resolve.
pub(crate) fn prefer_localisation_language_for_snapshot(
    snapshot: &AnalysisSnapshot,
    candidates: Vec<ResolutionDefinition>,
) -> Vec<ResolutionDefinition> {
    prefer_localisation_language_ordered(
        candidates,
        &[snapshot.localisation_preview_language(), "english"],
    )
}

fn prefer_localisation_language_ordered(
    candidates: Vec<ResolutionDefinition>,
    languages: &[impl AsRef<str>],
) -> Vec<ResolutionDefinition> {
    if candidates.len() < 2 {
        return candidates;
    }
    for language in languages {
        let preferred = candidates
            .iter()
            .filter(|candidate| {
                localisation_language(candidate.location.path.as_ref())
                    .is_some_and(|value| value.eq_ignore_ascii_case(language.as_ref()))
            })
            .cloned()
            .collect::<Vec<_>>();
        if !preferred.is_empty() {
            return preferred;
        }
    }
    candidates
}

/// Picks the effective localisation definition from a candidate list sorted by
/// priority descending then read order ascending: the highest layer wins, and
/// within that layer the latest-read definition wins — mirroring the game's
/// later-load override semantics.
/// Equal-priority runs are contiguous in the sorted list, so the last element
/// of the leading run is the winner.
pub(crate) fn effective_localisation_candidate(
    candidates: &[ResolutionDefinition],
) -> Option<&ResolutionDefinition> {
    let highest_priority = candidates.first()?.priority;
    candidates
        .iter()
        .take_while(|candidate| candidate.priority == highest_priority)
        .last()
}

/// Resolves localisation keys to their effective displayed value — exactly one
/// value per key, in the workspace target language: candidates are
/// language-filtered, then the highest layer wins and within that layer the
/// latest read order wins (the game's later-load override semantics). Keys
/// with no target-language definition are simply absent from the map.
///
/// Used by the LSP mission preview to resolve mission titles (`{id}_title`).
/// Each key is resolved with a targeted overlay scan plus an exact index lookup
/// (mirroring `symbol_candidates_for_hover`), so a request never rebuilds the
/// full workspace semantics — resolving a handful of titles against an
/// EU4-scale index stays cheap.
pub fn localisation_values_by_key<'a>(
    snapshot: &AnalysisSnapshot,
    keys: &'a [&'a str],
    cancellation: &CancellationToken,
) -> Result<HashMap<String, (Option<String>, String)>, Cancelled> {
    let target = snapshot.localisation_preview_language();
    let mut resolved = HashMap::new();
    for &key in keys {
        cancellation.checkpoint()?;
        let candidates = symbol_candidates_for_hover(snapshot, "localisation", key, cancellation)?;
        let Some(definition) = effective_localisation_candidate(&candidates) else {
            continue;
        };
        let Some((language, value)) =
            crate::localisation::localisation_preview(snapshot, definition)
        else {
            continue;
        };
        if crate::localisation::preview_language_is_target(language.as_deref(), target) {
            resolved.insert(key.to_owned(), (language, value));
        }
    }
    Ok(resolved)
}

pub(crate) struct DirectResolutionContext<'snapshot> {
    pub(crate) snapshot: &'snapshot AnalysisSnapshot,
    pub(crate) overlay_files: BTreeSet<SourceFileId>,
    pub(crate) overlay_definitions: BTreeMap<(String, String), Vec<ResolutionDefinition>>,
}

impl<'snapshot> DirectResolutionContext<'snapshot> {
    pub(crate) fn new(snapshot: &'snapshot AnalysisSnapshot) -> Self {
        let mut context = Self {
            snapshot,
            overlay_files: BTreeSet::new(),
            overlay_definitions: BTreeMap::new(),
        };
        let suppressed = twin_suppressed_overlays(snapshot);
        for document in snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
        {
            if let Some(path) = document.path()
                && let Some(file) = snapshot.source_file_id_for_path(path)
            {
                context.overlay_files.insert(file);
            }
            if suppressed.contains(document.id()) {
                continue;
            }
            let Some(input) = input_for_document(snapshot, document.id()) else {
                continue;
            };
            for definition in semantic_data(snapshot, &input).definitions {
                let priority = definition_priority(snapshot, &definition);
                context
                    .overlay_definitions
                    .entry((
                        definition.kind.to_ascii_lowercase(),
                        definition.name.to_ascii_lowercase(),
                    ))
                    .or_default()
                    .push(ResolutionDefinition {
                        location: definition.symbol.location,
                        selection_range: definition.symbol.selection_range,
                        priority,
                    });
            }
        }
        context
    }

    pub(crate) fn resolve(&self, kind: &str, name: &str) -> Resolution {
        let candidates = self.candidates(kind, name);
        if candidates.is_empty() {
            return Resolution::Missing;
        }
        let mut ordered = retain_highest_and_order(candidates);
        match ordered.pop() {
            Some(winner) => Resolution::Unique(winner),
            None => Resolution::Missing,
        }
    }

    /// Collects every candidate for one symbol, applying the localisation
    /// language preference.
    fn candidates(&self, kind: &str, name: &str) -> Vec<ResolutionDefinition> {
        let mut candidates = if self.overlay_definitions.is_empty() {
            Vec::new()
        } else {
            self.overlay_definitions
                .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_default()
        };
        candidates.extend(
            self.snapshot
                .index()
                .definitions(kind, name)
                .into_iter()
                .filter(|definition| !self.overlay_files.contains(&definition.file_id))
                .map(|definition| index_definition(self.snapshot, definition)),
        );
        if kind.eq_ignore_ascii_case("localisation") {
            candidates = prefer_localisation_language_for_snapshot(self.snapshot, candidates);
        }
        candidates
    }

    /// Returns the load-ordered candidates whose priority can actually win.
    pub(crate) fn ordered_candidates(&self, kind: &str, name: &str) -> Vec<ResolutionDefinition> {
        retain_highest_and_order(self.candidates(kind, name))
    }
}

/// Overlay documents suppressed by a twin over the same backing path.
///
/// The extension's decoded view is a `pdcloc://` twin of the raw `file://`
/// document, and a tab takeover transiently opens both twins (a window reload
/// can even restore both persistently). Storage keeps every open document —
/// the client owns their lifetimes — but one file must contribute one
/// effective text: counting both twins makes every definition in the file
/// shadow itself. The winner is the decoded twin (the surface being edited);
/// same-scheme spelling twins fall back to id order for determinism. The
/// losers' disk shards stay hidden either way: `overlay_files` keys paths.
fn twin_suppressed_overlays(snapshot: &AnalysisSnapshot) -> BTreeSet<DocumentId> {
    let mut owner: HashMap<&std::path::Path, &DocumentId> = HashMap::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(path) = document.path() else {
            continue;
        };
        match owner.get(path) {
            Some(current) if !overlay_twin_replaces(current, document.id()) => {}
            _ => {
                owner.insert(path, document.id());
            }
        }
    }
    snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| {
            let path = document.path()?;
            owner
                .get(path)
                .is_some_and(|owner| *owner != document.id())
                .then(|| document.id().clone())
        })
        .collect()
}

/// Whether `candidate` takes over the effective-text role of `current` for
/// one backing path: a decoded `pdcloc://` twin outranks its raw `file://`
/// twin; twins sharing a scheme resolve to id order so the winner never
/// depends on map iteration order.
fn overlay_twin_replaces(current: &DocumentId, candidate: &DocumentId) -> bool {
    match (
        is_decoded_view_uri(current.as_str()),
        is_decoded_view_uri(candidate.as_str()),
    ) {
        (false, true) => true,
        (true, false) => false,
        _ => candidate > current,
    }
}

fn is_decoded_view_uri(uri: &str) -> bool {
    uri.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("pdcloc"))
}

/// Retains the highest-priority candidates and orders them oldest-first so the
/// last entry is the effective definition under the game's
/// later-definition-wins rule. Priorities order candidates across source
/// roots; within one priority, file paths and in-file positions approximate
/// the load order.
pub(crate) fn retain_highest_and_order(
    mut candidates: Vec<ResolutionDefinition>,
) -> Vec<ResolutionDefinition> {
    let highest = candidates
        .iter()
        .map(|candidate| candidate.priority)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.priority == highest);
    candidates.sort_by(|left, right| {
        symbol_location_sort_key(&left.location)
            .cmp(&symbol_location_sort_key(&right.location))
            .then_with(|| {
                left.selection_range
                    .start()
                    .cmp(&right.selection_range.start())
            })
    });
    candidates
}

pub(crate) fn definition_priority(snapshot: &AnalysisSnapshot, definition: &DefinitionInfo) -> u64 {
    if definition.document.is_some() {
        return 20_000;
    }
    let Some(file) = definition
        .file
        .and_then(|id| snapshot.source_files().get(&id))
    else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        engine::SourceRootKind::Vanilla => 0,
        engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        engine::SourceRootKind::Project => 10_000 + u64::from(root.order),
    }
}

pub(crate) fn index_definition(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> ResolutionDefinition {
    let (path, document) = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| (Some(file.logical_path.clone()), None))
        .unwrap_or((None, None));
    ResolutionDefinition {
        location: Location {
            document,
            file: Some(definition.file_id),
            path,
            range: definition.range,
        },
        selection_range: indexed_definition_selection_range(definition),
        priority: definition_priority_for_file(snapshot, definition.file_id),
    }
}

/// Converts one persisted reference into an editor-neutral location, retaining the same dynamic
/// callability filters used when exhaustive semantic workspaces were built.
pub(crate) fn indexed_reference(
    snapshot: &AnalysisSnapshot,
    file_id: SourceFileId,
    reference: &Reference,
) -> Option<ReferenceInternal> {
    let path = snapshot
        .source_files()
        .get(&file_id)
        .map(|file| file.logical_path.clone());
    Some(ReferenceInternal {
        kind: reference.kind.to_string(),
        name: reference.name.to_string(),
        range: reference.range,
        document: None,
        file: Some(file_id),
        path,
    })
}

pub(crate) fn index_definition_info(
    snapshot: &AnalysisSnapshot,
    definition: &Definition,
) -> DefinitionInfo {
    let selection_range = indexed_definition_selection_range(definition);
    let path = snapshot
        .source_files()
        .get(&definition.file_id)
        .map(|file| file.logical_path.clone());
    let location = Location {
        document: None,
        file: Some(definition.file_id),
        path,
        range: definition.range,
    };
    DefinitionInfo {
        kind: definition.kind.to_string(),
        name: definition.name.to_string(),
        symbol: Symbol {
            name: definition.name.to_string(),
            kind: definition.kind.to_string(),
            range: definition.range,
            selection_range,
            location,
        },
        document: None,
        file: Some(definition.file_id),
    }
}

pub(crate) fn indexed_definition_selection_range(definition: &Definition) -> TextRange {
    definition.selection_range
}

pub(crate) fn definition_selection_location(definition: &ResolutionDefinition) -> Location {
    let mut location = definition.location.clone();
    location.range = definition.selection_range;
    location
}

pub(crate) fn definition_priority_for_file(snapshot: &AnalysisSnapshot, id: SourceFileId) -> u64 {
    let Some(file) = snapshot.source_files().get(&id) else {
        return 0;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return 0;
    };
    match root.kind {
        engine::SourceRootKind::Vanilla => 0,
        engine::SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        engine::SourceRootKind::Project => 10_000 + u64::from(root.order),
    }
}
pub(crate) fn symbol_at(
    all: &SemanticWorkspace,
    document: &DocumentId,
    position: TextSize,
) -> Option<(String, String)> {
    if let Some(reference) = all.references.iter().find(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    }) {
        return Some((reference.kind.clone(), reference.name.clone()));
    }
    all.definitions
        .iter()
        .find(|definition| {
            definition.document.as_ref() == Some(document)
                && contains(definition.symbol.selection_range, position)
        })
        .map(|definition| (definition.kind.clone(), definition.name.clone()))
}

pub(crate) fn local_parameter_target(
    input: &ParsedInput,
    position: TextSize,
) -> Option<(&hir::HirParameterDefinition, &hir::HirParameterReference)> {
    let hir = input.hir.as_deref()?;
    let reference = hir.parameter_reference_at(position)?;
    let definition = hir
        .parameter_definitions_for_owner(reference.owner_range)
        .find(|definition| definition.name.eq_ignore_ascii_case(&reference.name))?;
    Some((definition, reference))
}
