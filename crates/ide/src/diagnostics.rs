use crate::dynamic_contracts;
use crate::dynamic_cycles;
use crate::messages::did_you_mean;
use crate::resolution::*;
use crate::semantic::*;
use crate::suggest::best_suggestion;
use crate::support::*;
use crate::types::*;
use engine::{AnalysisSnapshot, DocumentId, DocumentSource, SourceFileId};
use hir::Scope;
use parser::{FileFormat, SyntaxError};
use std::sync::Arc;
use text::TextRange;

/// Single finalization point for diagnostics emitted by syntax, semantic, and reference passes.
///
/// Keeping deduplication here prevents each producer from inventing slightly different identity
/// rules. Until every producer exposes a structured subject (the exact symbol or key), the
/// message remains part of identity so two independent symbol failures sharing a range survive.
#[derive(Default)]
struct DiagnosticCollector {
    values: Vec<Diagnostic>,
}

impl DiagnosticCollector {
    fn new(values: Vec<Diagnostic>) -> Self {
        Self { values }
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        self.values.push(diagnostic);
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        self.values.sort_by_key(|diagnostic| {
            (
                diagnostic.range.start(),
                diagnostic.range.end(),
                diagnostic.code,
                diagnostic.severity,
            )
        });
        self.values.dedup_by(|left, right| {
            if left.code == right.code && left.range == right.range && left.message == right.message
            {
                for fix in right.fixes.drain(..) {
                    if !left.fixes.contains(&fix) {
                        left.fixes.push(fix);
                    }
                }
                for related in right.related.drain(..) {
                    if !left.related.contains(&related) {
                        left.related.push(related);
                    }
                }
                left.notes.append(&mut right.notes);
                true
            } else {
                false
            }
        });
        self.values
    }
}

fn file_analysis(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    semantic: SemanticFile,
    diagnostics: Vec<Diagnostic>,
) -> FileAnalysis {
    let mut coverage = input
        .hir
        .as_ref()
        .map(|hir| hir.analysis_coverage().clone())
        .unwrap_or_default();
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::AnalysisIncomplete)
    {
        coverage
            .limits
            .insert(hir::analysis::AnalysisLimit::DependentQuery);
    }
    FileAnalysis {
        coverage,
        revision: snapshot.revision(),
        document: input.document.clone(),
        file: input.file,
        format: Some(input.format),
        scope: Scope::Unknown,
        diagnostics,
        symbols: semantic
            .definitions
            .into_iter()
            .map(|definition| definition.symbol)
            .collect(),
        references: semantic
            .references
            .into_iter()
            .map(|reference| {
                let location = reference.location();
                ReferenceInfo {
                    kind: reference.kind,
                    name: reference.name,
                    location,
                }
            })
            .collect(),
    }
}

/// Runs diagnostics for all open overlays.  Disk-only files are intentionally excluded from push
/// diagnostics; they still participate in navigation and workspace-symbol queries.
#[must_use]
pub fn analyze(snapshot: &AnalysisSnapshot) -> AnalysisResult {
    let mut diagnostics = Vec::new();
    for document in snapshot.documents().values() {
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(analysis) = analyze_document(snapshot, document.id()) {
            diagnostics.extend(analysis.diagnostics);
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.code,
        )
    });
    AnalysisResult {
        revision: snapshot.revision(),
        scope: Scope::Unknown,
        diagnostics,
    }
}

/// Analyses one open or disk-backed document.
#[must_use]
pub fn analyze_document(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
) -> Option<FileAnalysis> {
    let input = input_for_document(snapshot, document)?;
    Some(analyze_input(snapshot, &input))
}

/// Analyses one indexed disk file.
#[must_use]
pub fn analyze_source_file(
    snapshot: &AnalysisSnapshot,
    file: SourceFileId,
) -> Option<FileAnalysis> {
    let input = input_for_source_file(snapshot, file)?;
    Some(analyze_input(snapshot, &input))
}

/// Returns diagnostics for one indexed disk file while cooperatively observing cancellation.
pub fn source_file_diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    file: SourceFileId,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    // Prose files deliberately publish no diagnostics. A diagnostics-only
    // workspace sweep need not reparse and lower their potentially large HIR;
    // full file analysis still extracts symbols for navigation and hover.
    if snapshot.source_files().get(&file).is_some_and(|source| {
        snapshot
            .rules()
            .classify(&source.logical_path)
            .is_some_and(|category| category.parser == rules::ParserKind::Localisation)
    }) {
        return Ok(Vec::new());
    }
    let Some(input) = crate::support::diagnostic_input_for_source_file(snapshot, file) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}

/// Returns diagnostics for caller-supplied text classified by its logical path.
///
/// This query does not mutate the workspace or create an overlay. It is intended for bounded
/// batch tooling whose backing files already participate in the immutable snapshot index.
pub fn text_diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    path: &text::LogicalPath,
    text: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_text(snapshot, path, text) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}

/// Returns diagnostics for one document, or an empty vector for unsupported/nonexistent files.
#[must_use]
pub fn diagnostics(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Diagnostic> {
    uncancelled(diagnostics_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns diagnostics while cooperatively stopping when `cancellation` is marked.
pub fn diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}
pub(crate) fn analyze_input(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> FileAnalysis {
    uncancelled(analyze_input_with_cancellation(
        snapshot,
        input,
        &CancellationToken::new(),
    ))
}

pub(crate) fn analyze_input_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<FileAnalysis, Cancelled> {
    cancellation.checkpoint()?;
    let semantic = semantic_data(snapshot, input);
    cancellation.checkpoint()?;
    // Localisation documents remain parsed and indexed so script-side references, hover, and
    // navigation keep working, but the editor surface deliberately publishes no diagnostics for
    // prose files.  Diagnostics below are reserved for Paradox script documents.
    if input.format == FileFormat::Localisation {
        return Ok(file_analysis(snapshot, input, semantic, Vec::new()));
    }
    let resolution = DirectResolutionContext::new(snapshot);
    let mut diagnostics = DiagnosticCollector::new(syntax_diagnostics(input));
    let ir_schema_path = crate::ir_semantic::has_ir_schema(snapshot, input);
    {
        diagnostics
            .values
            .extend(dynamic_contracts::dynamic_contract_diagnostics(
                snapshot,
                input,
                cancellation,
            )?);
        diagnostics
            .values
            .extend(dynamic_contracts::dynamic_call_site_diagnostics(
                snapshot,
                input,
                cancellation,
            )?);
        diagnostics
            .values
            .extend(dynamic_cycles::dynamic_cycle_diagnostics(
                snapshot,
                input,
                cancellation,
            )?);
        diagnostics.values.extend(crate::ir_semantic::diagnostics(
            snapshot,
            input,
            cancellation,
        )?);
        diagnostics
            .values
            .extend(crate::ir_queries::modifier_scope_diagnostics(
                snapshot,
                input,
                cancellation,
            )?);
    }
    diagnostics
        .values
        .extend(crate::transcode::transcode_diagnostics(
            input,
            cancellation,
        )?);
    let mission = crate::mission::mission_diagnostics(snapshot, input, cancellation)?;
    if !mission.is_empty() {
        // The mission validator explains a dangling prerequisite with full
        // context ("mission A requires unknown mission B") and underlines the
        // exact prerequisite token; drop the generic bare-value complaint
        // covering the same token so users see one diagnostic per fault.
        let dependency_ranges: std::collections::HashSet<TextRange> = mission
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::InvalidDependency)
            .map(|diagnostic| diagnostic.range)
            .collect();
        diagnostics.values.retain(|diagnostic| {
            !(diagnostic.code == DiagnosticCode::InvalidValue
                && dependency_ranges.contains(&diagnostic.range))
        });
        diagnostics.values.extend(mission);
    }
    // Localisation existence is judged in the workspace's target preview language, not
    // across all languages: a key that only exists in another language renders as its
    // raw spelling in-game and is reported. One batched resolution serves every
    // localisation reference below.
    let mut localisation_keys = semantic
        .references
        .iter()
        .filter(|reference| {
            reference.kind.eq_ignore_ascii_case("localisation") && !reference.name.contains('$')
        })
        .map(|reference| reference.name.as_str())
        .collect::<Vec<_>>();
    localisation_keys.sort_unstable();
    localisation_keys.dedup();
    let localisation_target_values =
        crate::resolution::localisation_values_by_key(snapshot, &localisation_keys, cancellation)?;
    for reference in &semantic.references {
        cancellation.checkpoint()?;
        if reference.name.contains('$') {
            continue;
        }
        if reference.kind.eq_ignore_ascii_case("localisation") {
            // Scalar arguments inside a dynamic definition invocation are untyped
            // parameter values, not localisation key references.
            if localisation_reference_is_dynamic_argument(snapshot, input, reference.range) {
                continue;
            }
            if !localisation_target_values.contains_key(&reference.name) {
                // The game renders a missing localisation key as its raw spelling,
                // so a missing key is a data-quality warning rather than a script
                // error. A key that exists only in other languages gets its own
                // message naming the target language, so the did-you-mean hint
                // never suggests the key itself.
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::UnknownLocalisationKey,
                    DiagnosticCode::UnknownLocalisationKey.severity(),
                    reference.range,
                    match resolution.resolve("localisation", &reference.name) {
                        Resolution::Unique(definition) => {
                            let defined = crate::resolution::localisation_language(
                                definition.location.path.as_ref(),
                            )
                            .map_or_else(String::new, |language| {
                                format!(" (defined in {language})")
                            });
                            format!(
                                "localisation key `{}` is missing in {}{}",
                                reference.name,
                                snapshot.localisation_preview_language(),
                                defined
                            )
                        }
                        Resolution::Missing => format!(
                            "unknown localisation key `{}`{}",
                            reference.name,
                            did_you_mean(
                                localisation_key_suggestion(snapshot, &reference.name).as_deref()
                            )
                        ),
                    },
                ));
            }
            continue;
        }
        match resolution.resolve(&reference.kind, &reference.name) {
            // `on_trigger` and `_trigger`-suffixed carriers may name fixed builtin
            // triggers, not only workspace scripted triggers; likewise for effects.
            Resolution::Missing
                if (reference.kind.eq_ignore_ascii_case("scripted_trigger")
                    && builtin_rule_has_key(snapshot, "trigger", &reference.name))
                    || (reference.kind.eq_ignore_ascii_case("scripted_effect")
                        && builtin_rule_has_key(snapshot, "effect", &reference.name)) => {}
            Resolution::Missing
                if ir_schema_path
                    && snapshot
                        .ir()
                        .type_by_name(&reference.kind)
                        .is_some_and(|type_id| {
                            rules::ir::SymbolFacts::type_member(
                                &crate::ir_queries::SnapshotSymbolFacts { snapshot },
                                type_id,
                                &reference.name,
                            )
                        }) => {}
            Resolution::Missing => {
                let ir_already_explains_value = ir_schema_path
                    && diagnostics.values.iter().any(|diagnostic| {
                        diagnostic.code == DiagnosticCode::InvalidValue
                            && diagnostic.range == reference.range
                    });
                if !ir_already_explains_value {
                    diagnostics.push(Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        DiagnosticCode::InvalidValue.severity(),
                        reference.range,
                        format!(
                            "unknown {} `{}`{}",
                            reference.kind,
                            reference.name,
                            did_you_mean(best_suggestion(
                                &reference.name,
                                effective_workspace_member_names(snapshot, &reference.kind)
                                    .iter()
                                    .map(String::as_str)
                            ))
                        ),
                    ));
                }
            }
            Resolution::Unique(_) => {}
        }
    }
    for definition in &semantic.definitions {
        cancellation.checkpoint()?;
        if definition.name.contains('$') {
            continue;
        }
        // Shadowing is only meaningful for kinds the game resolves by name
        // (later definition wins). The index also harvests member definitions
        // named after structural keys (`maneuver`, `graphical_culture`, event
        // flags) that repeat legally in every instance; those never conflict.
        let resolves_by_name = {
            snapshot
                .ir()
                .type_by_name(&definition.kind)
                .is_some_and(|type_id| {
                    snapshot.ir().type_info(type_id).resolution
                        == rules::ir::TypeResolution::Replace
                })
        };
        if !resolves_by_name {
            continue;
        }
        let ordered = resolution.ordered_candidates(&definition.kind, &definition.name);
        // Candidates above were already reduced to one priority, so any earlier
        // sibling here comes from the same source root; cross-root overrides
        // (for example a mod replacing a vanilla definition) stay silent.
        let Some(index) = ordered
            .iter()
            .position(|candidate| candidate.location == definition.symbol.location)
        else {
            continue;
        };
        if index > 0 {
            let mut diagnostic = Diagnostic::new(
                DiagnosticCode::AmbiguousDefinition,
                DiagnosticCode::AmbiguousDefinition.severity(),
                definition.symbol.selection_range,
                format!(
                    "definition `{}` shadows an earlier definition of the same name; the later definition takes effect",
                    definition.name
                ),
            );
            // Point at the definition being shadowed so one click separates a
            // true collision from an intentional override.
            if let Some(earlier) = index
                .checked_sub(1)
                .and_then(|earlier| ordered.get(earlier))
            {
                let mut location = earlier.location.clone();
                location.range = earlier.selection_range;
                diagnostic = diagnostic.with_related(RelatedLocation {
                    location,
                    message: format!("earlier definition of `{}`", definition.name),
                });
            }
            diagnostics.push(diagnostic);
        }
    }
    let diagnostics = diagnostics.finish();
    cancellation.checkpoint()?;
    Ok(file_analysis(snapshot, input, semantic, diagnostics))
}

fn builtin_rule_has_key(snapshot: &AnalysisSnapshot, context: &str, key: &str) -> bool {
    let ir = snapshot.ir();
    ir.schema_by_name(context).is_some_and(|schema|ir.fields(schema).iter().any(|id|
        matches!(ir.matcher(ir.field(*id).key),rules::ir::Matcher::Literal(name) if ir.strings().resolve(*name).eq_ignore_ascii_case(key))))
}

/// Returns whether a localisation-kind reference sits inside a dynamic definition
/// invocation, where scalar arguments are untyped parameter values.
fn localisation_reference_is_dynamic_argument(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    range: TextRange,
) -> bool {
    let Some(hir) = input.hir.as_deref() else {
        return false;
    };
    let Some(property) = hir.properties().iter().rfind(|property| {
        property.range.start() <= range.start() && property.range.end() >= range.end()
    }) else {
        return false;
    };
    // A definition body (path length 2) keeps localisation validation; only
    // nested invocations carry opaque dynamic arguments.
    if property.path.len() < 3 {
        return false;
    }
    let Some(parent_key) = property.path.iter().rev().nth(1) else {
        return false;
    };
    workspace_member(snapshot, "scripted_effect", parent_key)
        || workspace_member(snapshot, "scripted_trigger", parent_key)
}

/// Finds a unique close indexed localisation key for a missing reference.
///
/// Memoized per snapshot revision for the same reason as
/// [`enum_value_suggestion`]: a bulk pass reports the same missing keys across
/// many files, and each lookup otherwise walks the whole localisation key
/// universe with bounded edit distance.
fn localisation_key_suggestion(snapshot: &AnalysisSnapshot, name: &str) -> Option<String> {
    let revision = snapshot.revision();
    let lowered = name.to_ascii_lowercase();
    if let Some(cached) = probe_query_cache::<Option<String>>(
        snapshot,
        revision,
        &["localisation-key-suggestion:", &lowered],
    ) {
        return cached.as_ref().clone();
    }
    let suggestion = best_suggestion(name, localisation_key_index(snapshot).candidates(name, 2))
        .map(str::to_owned);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Documents,
        format!("localisation-key-suggestion:{}", lowered),
        Arc::new(suggestion.clone()),
    );
    suggestion
}

pub(crate) fn syntax_diagnostics(input: &ParsedInput) -> Vec<Diagnostic> {
    match &input.parsed {
        ParsedContent::Text(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
    }
}

pub(crate) fn diagnostic_from_syntax(error: &SyntaxError) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::Syntax,
        DiagnosticCode::Syntax.severity(),
        error.range,
        error.message.clone(),
    )
}
