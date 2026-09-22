use super::*;
use crate::MAX_WORKSPACE_DIAGNOSTIC_PUBLICATIONS;
use crate::uri::FileUri;
use std::fs;

/// Serializes workspace diagnostics once and hashes the bytes so the publish
/// loop can diff by hash and reuse the payload without rebuilding a Value tree.
fn encode_workspace_publication<T: serde::Serialize>(
    values: T,
) -> Result<(std::sync::Arc<serde_json::value::RawValue>, u64), serde_json::Error> {
    use std::hash::{Hash, Hasher};
    let bytes = serde_json::to_vec(&values)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let hash = hasher.finish();
    // serde_json 1.0.151 只有 from_string；to_vec 的产物必为合法 UTF-8
    let json = String::from_utf8(bytes)
        .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))?;
    let payload = std::sync::Arc::from(serde_json::value::RawValue::from_string(json)?);
    Ok((payload, hash))
}

/// Cheap in-memory content hash used by the diagnostics cache key.
fn content_fingerprint(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Fingerprint of the diagnostics filters applied to every publication, so a
/// settings change (ignored codes, severity overrides) invalidates the cache
/// even though content and context are unchanged.
fn diagnostics_filters_fingerprint(
    ignored_diagnostic_codes: &HashSet<String>,
    diagnostic_severity_overrides: &BTreeMap<String, Option<Severity>>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut codes = ignored_diagnostic_codes.iter().collect::<Vec<_>>();
    codes.sort();
    for code in codes {
        code.hash(&mut hasher);
    }
    for (code, severity) in diagnostic_severity_overrides {
        code.hash(&mut hasher);
        format!("{severity:?}").hash(&mut hasher);
    }
    hasher.finish()
}

/// Validates every parsed Project source file in a refreshed candidate and aggregates the
/// result for the explicit `validateWorkspace` command. The source-root refresh has already
/// produced a deterministic file set; sorting here keeps the cancellation and count semantics
/// stable even when the underlying map representation changes.
fn workspace_validation_result(
    host: &AnalysisHost,
    scan_cancellation: &WorkspaceScanToken,
    ignored_diagnostic_codes: &HashSet<String>,
    diagnostic_severity_overrides: &BTreeMap<String, Option<Severity>>,
    publish_diagnostics: bool,
    diagnostics_cache: &std::sync::Arc<WorkspaceDiagnosticsCache>,
) -> Result<WorkspaceValidationResult, WorkspaceError> {
    let snapshot = host.snapshot();
    let base_revision = snapshot.revision();
    let mut files = snapshot
        .source_files()
        .values()
        .filter(|file| {
            snapshot
                .source_roots()
                .iter()
                .any(|root| root.id == file.root_id && root.kind == SourceRootKind::Project)
        })
        .filter(|file| {
            // Frontends may have been evicted after an earlier validation
            // pass; parseability is a property of the file's rule category,
            // not of the currently retained tree. Evicted files reparse on
            // demand inside the per-file diagnostics call.
            snapshot.file_state(file.id).is_some()
                && snapshot
                    .rules()
                    .classify(&file.logical_path)
                    .is_some_and(|category| {
                        matches!(
                            category.parser,
                            rules::ParserKind::Script | rules::ParserKind::Localisation
                        )
                    })
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.logical_path
            .as_str()
            .cmp(right.logical_path.as_str())
            .then_with(|| left.physical_path.cmp(&right.physical_path))
    });

    let mut summary = WorkspaceValidationSummary {
        total_files: files.len(),
        ..WorkspaceValidationSummary::default()
    };
    let open_documents = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path.clone().map(|path| (path, document)))
        .collect::<HashMap<_, _>>();
    let mut current_uris = Vec::new();
    let mut publications = Vec::new();
    let cancellation = CancellationToken::new();
    let mut published_files = 0usize;
    let mut cache_updates = Vec::new();
    let mut reused_files = 0usize;
    // Cache keys: one workspace context fingerprint for the whole pass plus a
    // per-file content hash. Computed once here so the workers only compare.
    let context_fingerprint = engine::workspace_context_fingerprint(&snapshot);
    let filters_fingerprint =
        diagnostics_filters_fingerprint(ignored_diagnostic_codes, diagnostic_severity_overrides);

    // Closed-file diagnostics are independent per file; computing them on one
    // thread made a full-mod pass take minutes of wall time (the same pass is
    // what both CWTools implementations parallelize). Diagnostics and line
    // indexes are computed on a bounded worker set; ordering-sensitive summary
    // counting and the bounded publication prefix are assembled sequentially
    // below from the per-index results.
    enum FileOutcome {
        Computed {
            filtered: Vec<ide::Diagnostic>,
            source: std::sync::Arc<str>,
            line_index: LineIndex,
            closed_uri: Option<String>,
            file_id: engine::SourceFileId,
            content: u64,
        },
        /// Cache hit: content, context, and filters all match the cached
        /// entry, so the counts are reused as-is and the payload (when the
        /// file belongs to the publication prefix) comes straight from the
        /// cache.
        Reused {
            uri: String,
            counts: crate::server::DiagnosticCounts,
            payload: Option<(std::sync::Arc<serde_json::value::RawValue>, u64)>,
        },
    }
    let outcomes = std::sync::Mutex::new((0..files.len()).map(|_| None).collect::<Vec<_>>());
    let write_outcome = |index: usize, outcome: FileOutcome| {
        outcomes.lock().expect("validation outcome lock poisoned")[index] = Some(outcome);
    };
    let next_file = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    // Four workers measured as the knee on a 7,907-file mod: wall time
    // saturates near 8 threads while CPU cost keeps climbing with contention,
    // so the default stays polite and PDC_VALIDATION_WORKERS overrides it.
    let worker_count = std::env::var("PDC_VALIDATION_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(crate::DEFAULT_WORKSPACE_VALIDATION_WORKERS)
        .clamp(1, crate::MAX_WORKSPACE_VALIDATION_WORKERS);
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed)
                        || scan_cancellation.is_cancelled()
                    {
                        return;
                    }
                    if host.live_revision() != base_revision {
                        // A newer revision superseded this pass's snapshot
                        // mid-flight (an edit or refresh committed while the
                        // pass walked). Its completion handler discards and
                        // reschedules on exactly this mismatch, so grinding on
                        // would burn hours of CPU for a result nobody reads;
                        // stop the whole pass now.
                        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    let index = next_file.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(file) = files.get(index) else {
                        return;
                    };
                    if let Some(document) = open_documents.get(&file.physical_path) {
                        let Ok(diagnostics) =
                            diagnostics_with_cancellation(&snapshot, document.id(), &cancellation)
                        else {
                            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                            return;
                        };
                        let source = document.text_handle();
                        let line_index = document.line_index().clone();
                        let filtered = filter_diagnostics_with_ignored_and_overrides(
                            diagnostics,
                            &line_index,
                            &source,
                            ignored_diagnostic_codes,
                            diagnostic_severity_overrides,
                        );
                        write_outcome(
                            index,
                            FileOutcome::Computed {
                                filtered,
                                source,
                                line_index,
                                closed_uri: None,
                                // Overlay documents are always recomputed; the
                                // id/content fields only feed cache updates,
                                // which overlays never produce.
                                file_id: file.id,
                                content: content_fingerprint(&document.text_handle()),
                            },
                        );
                        continue;
                    }
                    let Some(state) = snapshot.file_state(file.id) else {
                        continue;
                    };
                    // Cache hit: the file's own content, the workspace context,
                    // and the diagnostics filters all match the last published
                    // entry, so neither the reparse nor the recomputation can
                    // change the payload.
                    let content = content_fingerprint(state.source());
                    if let Some(uri) = FileUri::from_path(&file.physical_path)
                        .ok()
                        .map(|uri| uri.as_str().to_owned())
                        && let Some(entry) = diagnostics_cache
                            .read()
                            .expect("workspace diagnostics cache lock")
                            .get(&file.id)
                        && entry.content == content
                        && entry.context == context_fingerprint
                        && entry.filters == filters_fingerprint
                    {
                        write_outcome(
                            index,
                            FileOutcome::Reused {
                                uri,
                                counts: entry.counts,
                                payload: entry.payload.clone(),
                            },
                        );
                        continue;
                    }
                    let Ok(diagnostics) = source_file_diagnostics_with_cancellation(
                        &snapshot,
                        file.id,
                        &cancellation,
                    ) else {
                        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    };
                    let line_index = LineIndex::new(state.source());
                    let filtered = filter_diagnostics_with_ignored_and_overrides(
                        diagnostics,
                        &line_index,
                        &state.source_handle(),
                        ignored_diagnostic_codes,
                        diagnostic_severity_overrides,
                    );
                    write_outcome(
                        index,
                        FileOutcome::Computed {
                            filtered,
                            source: state.source_handle(),
                            line_index,
                            closed_uri: FileUri::from_path(&file.physical_path)
                                .ok()
                                .map(|uri| uri.as_str().to_owned()),
                            file_id: file.id,
                            content,
                        },
                    );
                }
            });
        }
    });
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) || scan_cancellation.is_cancelled() {
        return Err(WorkspaceError::Cancelled);
    }
    let outcomes = outcomes
        .into_inner()
        .expect("validation outcome lock poisoned");

    for outcome in outcomes.into_iter().flatten() {
        match outcome {
            FileOutcome::Reused {
                uri,
                counts,
                payload,
            } => {
                reused_files = reused_files.saturating_add(1);
                summary.validated_files = summary.validated_files.saturating_add(1);
                summary.total_errors = summary.total_errors.saturating_add(counts.errors);
                summary.total_warnings = summary.total_warnings.saturating_add(counts.warnings);
                summary.total_infos = summary.total_infos.saturating_add(counts.infos);
                summary.total_hints = summary.total_hints.saturating_add(counts.hints);
                if counts.has_error {
                    summary.files_with_errors = summary.files_with_errors.saturating_add(1);
                }
                current_uris.push(uri.clone());
                if publish_diagnostics
                    && published_files < MAX_WORKSPACE_DIAGNOSTIC_PUBLICATIONS
                    && let Some((payload, hash)) = payload
                {
                    publications.push(WorkspaceDiagnosticPublication { uri, payload, hash });
                    published_files = published_files.saturating_add(1);
                }
            }
            FileOutcome::Computed {
                filtered,
                source,
                line_index,
                closed_uri,
                file_id,
                content,
            } => {
                summary.validated_files = summary.validated_files.saturating_add(1);
                let mut file_has_error = false;
                let mut counts = crate::server::DiagnosticCounts {
                    errors: 0,
                    warnings: 0,
                    infos: 0,
                    hints: 0,
                    has_error: false,
                };
                for diagnostic in &filtered {
                    match diagnostic.severity {
                        Severity::Error => {
                            file_has_error = true;
                            counts.errors += 1;
                            summary.total_errors = summary.total_errors.saturating_add(1);
                        }
                        Severity::Warning => {
                            counts.warnings += 1;
                            summary.total_warnings = summary.total_warnings.saturating_add(1);
                        }
                        Severity::Information => {
                            counts.infos += 1;
                            summary.total_infos = summary.total_infos.saturating_add(1);
                        }
                        Severity::Hint => {
                            counts.hints += 1;
                            summary.total_hints = summary.total_hints.saturating_add(1);
                        }
                    }
                }
                counts.has_error = file_has_error;
                if file_has_error {
                    summary.files_with_errors = summary.files_with_errors.saturating_add(1);
                }
                if let Some(uri) = closed_uri {
                    current_uris.push(uri.clone());
                    let mut cached_payload = None;
                    if publish_diagnostics
                        && published_files < MAX_WORKSPACE_DIAGNOSTIC_PUBLICATIONS
                    {
                        let values = diagnostic_values_for_text_with_ignored_and_overrides(
                            filtered,
                            Some(&snapshot),
                            &line_index,
                            &source,
                            &HashSet::new(),
                            diagnostic_severity_overrides,
                        );
                        let payload = encode_workspace_publication(values).map_err(|error| {
                            WorkspaceError::Io(io::Error::other(format!(
                                "failed to serialize workspace diagnostics: {error}"
                            )))
                        })?;
                        publications.push(WorkspaceDiagnosticPublication {
                            uri,
                            payload: std::sync::Arc::clone(&payload.0),
                            hash: payload.1,
                        });
                        cached_payload = Some(payload);
                        published_files = published_files.saturating_add(1);
                    }
                    // Every computed closed file is cached so a no-change pass
                    // skips its reparse and recomputation entirely, not just
                    // the bounded publication prefix.
                    cache_updates.push((
                        file_id,
                        crate::server::CachedWorkspaceDiagnostics {
                            content,
                            context: context_fingerprint,
                            filters: filters_fingerprint,
                            counts,
                            payload: cached_payload,
                        },
                    ));
                }
            }
        }
    }
    Ok(WorkspaceValidationResult {
        summary,
        publications,
        current_uris,
        full_workspace: true,
        cache_updates,
        reused_files,
    })
}

/// Validates only the files touched by one watched-file batch instead of the
/// whole workspace. Deleted paths publish an empty diagnostic list so stale
/// entries clear. Cross-file impact — a change in one file altering another
/// file's diagnostics — is not tracked here; the explicit `validateWorkspace`
/// command and the post-ready full pass remain the full-fidelity paths.
fn changed_files_validation_result(
    host: &AnalysisHost,
    changes: &[DiskFileChange],
    scan_cancellation: &WorkspaceScanToken,
    ignored_diagnostic_codes: &HashSet<String>,
    diagnostic_severity_overrides: &BTreeMap<String, Option<Severity>>,
) -> Result<WorkspaceValidationResult, WorkspaceError> {
    let snapshot = host.snapshot();
    let open_documents = snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path.clone().map(|path| (path, document)))
        .collect::<HashMap<_, _>>();
    let mut summary = WorkspaceValidationSummary {
        total_files: changes.len(),
        ..WorkspaceValidationSummary::default()
    };
    let mut publications = Vec::new();
    let mut current_uris = Vec::new();
    let cancellation = CancellationToken::new();
    for change in changes {
        if scan_cancellation.is_cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        let uri = match FileUri::from_path(&change.path) {
            Ok(uri) => uri.as_str().to_owned(),
            Err(_) => continue,
        };
        if change.kind == DiskFileChangeKind::Deleted {
            current_uris.push(uri.clone());
            let payload = encode_workspace_publication(Vec::<Value>::new()).map_err(|error| {
                WorkspaceError::Io(io::Error::other(format!(
                    "failed to serialize changed-file diagnostics: {error}"
                )))
            })?;
            publications.push(WorkspaceDiagnosticPublication {
                uri,
                payload: payload.0,
                hash: payload.1,
            });
            summary.validated_files = summary.validated_files.saturating_add(1);
            continue;
        }
        let Some(file) = snapshot
            .source_files()
            .values()
            .find(|file| file.physical_path == change.path)
        else {
            continue;
        };
        if open_documents.contains_key(&file.physical_path) {
            // Open overlays are re-diagnosed by the document worker after the
            // host commit; publishing here would race its newer text.
            continue;
        }
        let Some(state) = snapshot.file_state(file.id) else {
            continue;
        };
        let diagnostics =
            source_file_diagnostics_with_cancellation(&snapshot, file.id, &cancellation)
                .map_err(|_| WorkspaceError::Cancelled)?;
        let line_index = LineIndex::new(state.source());
        let source = state.source_handle();
        let filtered = filter_diagnostics_with_ignored_and_overrides(
            diagnostics,
            &line_index,
            &source,
            ignored_diagnostic_codes,
            diagnostic_severity_overrides,
        );
        summary.validated_files = summary.validated_files.saturating_add(1);
        let mut file_has_error = false;
        for diagnostic in &filtered {
            match diagnostic.severity {
                Severity::Error => {
                    file_has_error = true;
                    summary.total_errors = summary.total_errors.saturating_add(1);
                }
                Severity::Warning => {
                    summary.total_warnings = summary.total_warnings.saturating_add(1);
                }
                Severity::Information => {
                    summary.total_infos = summary.total_infos.saturating_add(1);
                }
                Severity::Hint => {
                    summary.total_hints = summary.total_hints.saturating_add(1);
                }
            }
        }
        if file_has_error {
            summary.files_with_errors = summary.files_with_errors.saturating_add(1);
        }
        current_uris.push(uri.clone());
        let values = diagnostic_values_for_text_with_ignored_and_overrides(
            filtered,
            Some(&snapshot),
            &line_index,
            &source,
            &HashSet::new(),
            diagnostic_severity_overrides,
        );
        let payload = encode_workspace_publication(values).map_err(|error| {
            WorkspaceError::Io(io::Error::other(format!(
                "failed to serialize changed-file diagnostics: {error}"
            )))
        })?;
        publications.push(WorkspaceDiagnosticPublication {
            uri,
            payload: payload.0,
            hash: payload.1,
        });
    }
    Ok(WorkspaceValidationResult {
        summary,
        publications,
        current_uris,
        full_workspace: false,
        cache_updates: Vec::new(),
        reused_files: 0,
    })
}

/// Outcome of formatting one Project file during a `pdc/formatWorkspace` pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceFileFormatOutcome {
    Formatted,
    Unchanged,
    /// The parser reported errors or the rewrite failed safety validation; the
    /// file is left untouched.
    SkippedUnsafe,
    /// The on-disk bytes are not valid UTF-8 (legacy encoding). Rewriting the
    /// decoded text would silently re-encode the file, so it is left untouched.
    SkippedLegacyEncoding,
    Failed,
}

/// Applies the formatter's non-overlapping, source-ordered edits back-to-front.
fn apply_format_edits(source: &str, edits: &[parser::format::TextEdit]) -> String {
    let mut text = source.to_owned();
    for edit in edits.iter().rev() {
        if let (Some(start), Some(end)) = (
            usize::try_from(edit.range.start()).ok(),
            usize::try_from(edit.range.end()).ok(),
        ) && text.get(start..end).is_some()
        {
            text.replace_range(start..end, &edit.replacement);
        }
    }
    text
}

/// Formats one on-disk script file in place.
///
/// The file is re-read from disk rather than taken from the snapshot so the
/// rewrite always reflects the bytes the game sees. Files that are not valid
/// UTF-8 are skipped instead of re-encoded, and the formatter's own safety
/// pipeline (no parse errors, token equivalence, idempotence) gates every
/// write.
fn format_workspace_file(path: &AbsPath) -> WorkspaceFileFormatOutcome {
    let bytes = match fs::read(path.as_path()) {
        Ok(bytes) => bytes,
        Err(_) => return WorkspaceFileFormatOutcome::Failed,
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return WorkspaceFileFormatOutcome::SkippedLegacyEncoding;
    };
    let parsed = parser::parse(parser::FileFormat::Script, &text);
    let result = parser::format::format(&parsed);
    if result.skipped.is_some() {
        return WorkspaceFileFormatOutcome::SkippedUnsafe;
    }
    if result.edits.is_empty() {
        return WorkspaceFileFormatOutcome::Unchanged;
    }
    let formatted = apply_format_edits(&text, &result.edits);
    match fs::write(path.as_path(), formatted.as_bytes()) {
        Ok(()) => WorkspaceFileFormatOutcome::Formatted,
        Err(_) => WorkspaceFileFormatOutcome::Failed,
    }
}

/// Formats every Project script file in place and aggregates the outcome
/// for the explicit `pdc/formatWorkspace` command. Localisation files are out
/// of scope by design; vanilla and dependency roots are read-only reference
/// material and never written.
fn format_workspace_files(
    host: &AnalysisHost,
    scan_cancellation: &WorkspaceScanToken,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<WorkspaceFormatSummary, WorkspaceError> {
    let snapshot = host.snapshot();
    let mut files = snapshot
        .source_files()
        .values()
        .filter(|file| {
            snapshot
                .source_roots()
                .iter()
                .any(|root| root.id == file.root_id && root.kind == SourceRootKind::Project)
        })
        .filter(|file| {
            snapshot
                .rules()
                .classify(&file.logical_path)
                .is_some_and(|category| category.parser == rules::ParserKind::Script)
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.logical_path
            .as_str()
            .cmp(right.logical_path.as_str())
            .then_with(|| left.physical_path.cmp(&right.physical_path))
    });

    let mut summary = WorkspaceFormatSummary {
        total_files: files.len(),
        ..WorkspaceFormatSummary::default()
    };
    // Per-file formatting is independent disk-bound work; the bounded
    // worker-stealing pool mirrors `workspace_validation_result` so a full-mod
    // pass stays polite instead of saturating every core.
    let outcomes = std::sync::Mutex::new((0..files.len()).map(|_| None).collect::<Vec<_>>());
    let next_file = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let completed = std::sync::atomic::AtomicUsize::new(0);
    let reported = std::sync::atomic::AtomicUsize::new(0);
    let report_step = (files.len() / 50).max(1);
    let worker_count = std::env::var("PDC_FORMAT_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(crate::DEFAULT_WORKSPACE_FORMAT_WORKERS)
        .clamp(1, crate::MAX_WORKSPACE_FORMAT_WORKERS);
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed)
                        || scan_cancellation.is_cancelled()
                    {
                        return;
                    }
                    let index = next_file.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(file) = files.get(index) else {
                        return;
                    };
                    let outcome = format_workspace_file(&file.physical_path);
                    outcomes.lock().expect("format outcome lock poisoned")[index] = Some(outcome);
                    let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let last = reported.load(std::sync::atomic::Ordering::Relaxed);
                    if (done == files.len() || done.saturating_sub(last) >= report_step)
                        && reported
                            .compare_exchange(
                                last,
                                done,
                                std::sync::atomic::Ordering::Relaxed,
                                std::sync::atomic::Ordering::Relaxed,
                            )
                            .is_ok()
                    {
                        progress(done, files.len());
                    }
                }
            });
        }
    });
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) || scan_cancellation.is_cancelled() {
        return Err(WorkspaceError::Cancelled);
    }
    let outcomes = outcomes.into_inner().expect("format outcome lock poisoned");
    for outcome in outcomes.into_iter().flatten() {
        match outcome {
            WorkspaceFileFormatOutcome::Formatted => {
                summary.formatted_files = summary.formatted_files.saturating_add(1);
            }
            WorkspaceFileFormatOutcome::Unchanged => {
                summary.unchanged_files = summary.unchanged_files.saturating_add(1);
            }
            WorkspaceFileFormatOutcome::SkippedUnsafe => {
                summary.skipped_unsafe_files = summary.skipped_unsafe_files.saturating_add(1);
            }
            WorkspaceFileFormatOutcome::SkippedLegacyEncoding => {
                summary.skipped_legacy_encoding_files =
                    summary.skipped_legacy_encoding_files.saturating_add(1);
            }
            WorkspaceFileFormatOutcome::Failed => {
                summary.failed_files = summary.failed_files.saturating_add(1);
            }
        }
    }
    Ok(summary)
}

impl LspServer {
    /// Starts an explicit `workspace/executeCommand` refresh or validation request. Both commands
    /// share the same cloned-host and revision-checked commit path as the quiet pass, but are not
    /// idle gated.
    pub(super) fn spawn_reindex_command<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightReindexCommand>,
        busy: bool,
        message: &Value,
    ) -> bool {
        if self.state != ServerState::Initialized || in_flight.is_some() || busy {
            return false;
        }
        let Some(object) = message.as_object() else {
            return false;
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object.get("method").and_then(Value::as_str) != Some("workspace/executeCommand")
        {
            return false;
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return false;
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return false;
        };
        if self.cancelled.contains(&request_id) {
            // Let the ordinary dispatcher produce the standard cancellation response before a
            // worker is created for a request the client already abandoned.
            return false;
        }
        let Ok(params) =
            typed_params::<ExecuteCommandParams>(object.get("params"), "executeCommand")
        else {
            return false;
        };
        let command = match params.command.as_str() {
            "pdc/reindexWorkspace" | "reindexWorkspace" => WorkspaceCommand::Reindex,
            "pdc/validateWorkspace" | "validateWorkspace" => WorkspaceCommand::Validate,
            _ => return false,
        };
        let _ = params.arguments;
        self.mark_activity();

        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let publish_workspace_diagnostics = self.workspace_wide_diagnostics;
        let ignored_diagnostic_codes = Arc::clone(&self.ignored_diagnostic_codes);
        let diagnostics_cache = std::sync::Arc::clone(&self.workspace_diagnostics_cache);
        let diagnostic_severity_overrides = Arc::clone(&self.diagnostic_severity_overrides);
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        self.background_reindex_due = None;
        *in_flight = Some(InFlightReindexCommand {
            request_id: request_id.clone(),
            base_revision,
            command,
            cancellation,
        });
        let id = id.clone();
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                candidate
                    .refresh_source_roots_cancellable_with_progress(&worker_cancellation, None)
                    .and_then(|_| {
                        let summary = (command == WorkspaceCommand::Validate
                            || publish_workspace_diagnostics)
                            .then(|| {
                                workspace_validation_result(
                                    &candidate,
                                    &worker_cancellation,
                                    &ignored_diagnostic_codes,
                                    &diagnostic_severity_overrides,
                                    publish_workspace_diagnostics,
                                    &diagnostics_cache,
                                )
                            })
                            .transpose()?;
                        Ok((candidate, summary))
                    })
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace command worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::ReindexCommand(ReindexCommandResult {
                request_id,
                id,
                base_revision,
                command,
                result,
            }));
        });
        true
    }

    /// Starts an explicit `pdc/formatWorkspace` request. The worker formats
    /// Project script files straight to disk; completion never swaps the
    /// host — the watched-file pipeline and clean open-document reloads pick
    /// the rewrites up — so unlike the reindex commands there is no
    /// revision-checked commit to lose.
    pub(super) fn spawn_format_command<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightFormatCommand>,
        busy: bool,
        message: &Value,
        output: &mut W,
    ) -> Result<bool, LspError> {
        if self.state != ServerState::Initialized || in_flight.is_some() || busy {
            return Ok(false);
        }
        let Some(object) = message.as_object() else {
            return Ok(false);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object.get("method").and_then(Value::as_str) != Some("workspace/executeCommand")
        {
            return Ok(false);
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return Ok(false);
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return Ok(false);
        };
        if self.cancelled.contains(&request_id) {
            // Let the ordinary dispatcher produce the standard cancellation response before a
            // worker is created for a request the client already abandoned.
            return Ok(false);
        }
        let Ok(params) =
            typed_params::<ExecuteCommandParams>(object.get("params"), "executeCommand")
        else {
            return Ok(false);
        };
        if !matches!(
            params.command.as_str(),
            "pdc/formatWorkspace" | "formatWorkspace"
        ) {
            return Ok(false);
        }
        self.mark_activity();

        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let candidate = self.host.clone();
        let sender = event_sender.clone();
        self.background_reindex_due = None;
        // One progress token for the whole pass: the event loop sends the
        // terminal `end` report when the worker completes.
        let progress_token = format!("pdc-format-{}", progress_nonce());
        let client_progress = self.client_work_done_progress;
        if client_progress {
            write_message(output, &work_done_progress_create(&progress_token))?;
            write_message(
                output,
                &work_done_progress_begin(&progress_token, "Formatting workspace…"),
            )?;
        }
        *in_flight = Some(InFlightFormatCommand {
            request_id: request_id.clone(),
            cancellation,
            progress_token: client_progress.then_some(progress_token.clone()),
        });
        let progress = {
            let sender = sender.clone();
            let token = progress_token;
            move |done: usize, total: usize| {
                if !client_progress {
                    return;
                }
                let mut value = json!({
                    "kind": "report",
                    "message": format!("Formatting workspace ({done}/{total})…"),
                });
                if let Some(percent) = done
                    .checked_mul(100)
                    .and_then(|percent| percent.checked_div(total))
                {
                    value["percentage"] = json!(u32::try_from(percent).unwrap_or(100));
                }
                let _ = sender.send(TransportEvent::Progress(Progress {
                    params: json!({"token": token, "value": value}),
                }));
            }
        };
        let id = id.clone();
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                format_workspace_files(&candidate, &worker_cancellation, &progress)
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace format worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::FormatCommand(FormatCommandResult {
                request_id,
                id,
                result,
            }));
        });
        Ok(true)
    }

    /// Starts an automatic closed-file diagnostic pass once all foreground work has drained.
    ///
    /// This worker deliberately validates the current immutable snapshot without refreshing the
    /// source roots. Full refresh workers attach their own validation result so a whole-workspace
    /// re-scan never pays for a second full diagnostics walk; incremental watched-file batches
    /// cover only the files they touched and do not supersede this pass.
    pub(super) fn spawn_pending_workspace_diagnostics<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightWorkspaceDiagnostics>,
        busy: bool,
    ) {
        // ShuttingDown is accepted: the shutdown drain waits for this pass to
        // publish closed-file diagnostics queued by an in-flight scan's
        // completion, so refusing to spawn it would drop the publication.
        if !matches!(
            self.state,
            ServerState::Initialized | ServerState::ShuttingDown
        ) || !self.workspace_wide_diagnostics
            || !self.workspace_diagnostics_pending
            || in_flight.is_some()
            || busy
        {
            return;
        }
        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let ignored_diagnostic_codes = Arc::clone(&self.ignored_diagnostic_codes);
        let diagnostics_cache = std::sync::Arc::clone(&self.workspace_diagnostics_cache);
        let diagnostic_severity_overrides = Arc::clone(&self.diagnostic_severity_overrides);
        let candidate = self.host.clone();
        let sender = event_sender.clone();
        self.workspace_diagnostics_pending = false;
        *in_flight = Some(InFlightWorkspaceDiagnostics {
            base_revision,
            cancellation,
        });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                workspace_validation_result(
                    &candidate,
                    &worker_cancellation,
                    &ignored_diagnostic_codes,
                    &diagnostic_severity_overrides,
                    true,
                    &diagnostics_cache,
                )
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace diagnostics worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::WorkspaceDiagnostics(
                WorkspaceDiagnosticsResult {
                    base_revision,
                    result,
                },
            ));
        });
    }

    /// Starts a quiet full source-root refresh once its cadence and idle gate are satisfied.
    ///
    /// The worker owns a cloned host and therefore never holds the event-loop state while
    /// walking disk. The result is committed only when the base revision is still current; a
    /// foreground edit or watched-file refresh always wins.
    pub(super) fn spawn_due_background_reindex<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightBackgroundReindex>,
        ready: bool,
        busy: bool,
    ) {
        if !ready
            || self.state != ServerState::Initialized
            || self.background_reindex_interval().is_none()
            || in_flight.is_some()
        {
            return;
        }
        let Some(due) = self.background_reindex_due else {
            return;
        };
        let now = Instant::now();
        if due > now {
            return;
        }
        if now.duration_since(self.last_activity)
            < Duration::from_secs(self.background_reindex_idle_seconds)
        {
            return;
        }
        if busy {
            // A foreground scan/edit is already queued. Retry shortly after it completes rather
            // than spinning on an overdue deadline or competing with user-visible work.
            self.background_reindex_due = now.checked_add(Duration::from_secs(1));
            return;
        }

        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        let publish_workspace_diagnostics = self.workspace_wide_diagnostics;
        let ignored_diagnostic_codes = Arc::clone(&self.ignored_diagnostic_codes);
        let diagnostics_cache = std::sync::Arc::clone(&self.workspace_diagnostics_cache);
        let diagnostic_severity_overrides = Arc::clone(&self.diagnostic_severity_overrides);
        self.background_reindex_due = None;
        *in_flight = Some(InFlightBackgroundReindex {
            base_revision,
            cancellation,
        });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                candidate
                    .refresh_source_roots_cancellable_with_progress(&worker_cancellation, None)
                    .and_then(|_| {
                        let validation = publish_workspace_diagnostics
                            .then(|| {
                                workspace_validation_result(
                                    &candidate,
                                    &worker_cancellation,
                                    &ignored_diagnostic_codes,
                                    &diagnostic_severity_overrides,
                                    true,
                                    &diagnostics_cache,
                                )
                            })
                            .transpose()?;
                        Ok((candidate, validation))
                    })
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "background workspace reindex worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::BackgroundReindex(BackgroundReindexResult {
                base_revision,
                result,
            }));
        });
    }

    /// Computes the event-loop wait until the next quiet pass can be considered. A pending pass
    /// is still idle-gated, so the loop wakes at the remaining idle duration rather than polling
    /// a deadline that has already elapsed.
    pub(super) fn background_reindex_wait(
        &self,
        ready: bool,
        in_flight: Option<&InFlightBackgroundReindex>,
        busy: bool,
    ) -> Option<Duration> {
        if !ready
            || self.state != ServerState::Initialized
            || self.background_reindex_interval().is_none()
            || in_flight.is_some()
        {
            return None;
        }
        let due = self.background_reindex_due?;
        let now = Instant::now();
        if due > now {
            return Some(due.duration_since(now));
        }
        let idle = Duration::from_secs(self.background_reindex_idle_seconds);
        let elapsed = now.duration_since(self.last_activity);
        if elapsed < idle {
            return Some(idle - elapsed);
        }
        // Keep an overdue pass from causing a zero-timeout busy loop while another worker is
        // draining. The next normal event or this short retry will re-evaluate the guard.
        Some(if busy {
            Duration::from_secs(1)
        } else {
            Duration::ZERO
        })
    }

    pub(super) fn spawn_snapshot_request<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut HashMap<RequestId, InFlightRequest>,
        message: &Value,
    ) -> bool {
        if self.state != ServerState::Initialized {
            return false;
        }
        let Some(object) = message.as_object() else {
            return false;
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return false;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return false;
        };
        if !is_snapshot_request(method) {
            return false;
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return false;
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return false;
        };
        if in_flight.contains_key(&request_id) {
            return false;
        }

        let cancellation = CancellationToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        // Mirrors the event loop's inline dispatch: stage the disk text for
        // scanned files no editor opened before snapshotting the host.
        self.ensure_snapshot_request_document(object.get("params"));
        let context = SnapshotRequestContext::new(
            self.host.snapshot(),
            cancellation.clone(),
            self.client_snippet_support,
            Arc::clone(&self.ignored_diagnostic_codes),
            Arc::clone(&self.diagnostic_severity_overrides),
            Arc::clone(&self.semantic_tokens_cache),
        );
        let method = method.to_owned();
        let params = object.get("params").cloned();
        let id = id.clone();
        let sender = event_sender.clone();
        in_flight.insert(
            request_id.clone(),
            InFlightRequest {
                cancellation: cancellation.clone(),
            },
        );
        scope.spawn(move || {
            let result = if cancellation.is_cancelled() {
                Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
            } else {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.dispatch(&method, params.as_ref())
                }))
                .unwrap_or_else(|_| {
                    Err(RpcError::new(
                        INTERNAL_ERROR,
                        "request worker failed unexpectedly",
                    ))
                });
                if cancellation.is_cancelled() {
                    Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
                } else {
                    result
                }
            };
            let _ = sender.send(TransportEvent::Request(SnapshotRequestResult {
                request_id,
                id,
                result,
            }));
        });
        true
    }

    pub(super) fn spawn_initialize_request<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightInitialize>,
        message: &Value,
        output: &mut W,
        initialize_progress_token: &mut Option<String>,
    ) -> Result<bool, LspError> {
        if self.state != ServerState::Uninitialized || in_flight.is_some() {
            return Ok(false);
        }
        let Some(object) = message.as_object() else {
            return Ok(false);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object.get("method").and_then(Value::as_str) != Some("initialize")
        {
            return Ok(false);
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return Ok(false);
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return Ok(false);
        };
        let Ok(params) = typed_params::<InitializeParams>(object.get("params"), "initialize")
        else {
            return Ok(false);
        };
        let client_work_done_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);
        // Captured on the event-loop thread before the worker spawns so the
        // very first traced decisions of this initialize already observe it.
        if let Some(trace) = params.trace.as_ref() {
            self.client_trace = trace_value_string(trace);
        }

        let cancellation = WorkspaceScanToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        let candidate = self.host.clone();
        let scan_workspace = !self.host.snapshot().rules().game_id().is_empty();
        let auto_vanilla = self.auto_vanilla.clone();
        let startup_log = std::mem::take(&mut self.startup_log);
        let sender = event_sender.clone();
        let id = id.clone();
        let workspace_folder_count = params.workspace_folders.as_ref().map_or(0, Vec::len);
        let initialization_options = if params.initialization_options.is_some() {
            "present"
        } else {
            "absent"
        };
        let initialize_message = format!(
            "initialize request accepted: workspaceFolders={workspace_folder_count}, initializationOptions={initialization_options}, workDoneProgress={client_work_done_progress}, watchedFiles={}",
            params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .is_some()
        );
        // One progress token for the whole initialize phase: stage reports and
        // the workspace-scan counter share it, and the event loop sends the
        // terminal `end` report when the initialize worker completes.
        let progress_token = format!("pdc-init-{}", progress_nonce());
        if client_work_done_progress {
            write_message(output, &work_done_progress_create(&progress_token))?;
            write_message(
                output,
                &work_done_progress_begin(&progress_token, "Starting pdc…"),
            )?;
            *initialize_progress_token = Some(progress_token.clone());
        }
        let stage = {
            let sender = sender.clone();
            let token = progress_token.clone();
            move |message: &str| {
                if client_work_done_progress {
                    let _ = sender.send(TransportEvent::Progress(Progress {
                        params: json!({
                            "token": token,
                            "value": {"kind": "report", "message": message},
                        }),
                    }));
                }
            }
        };
        let log = {
            let sender = sender.clone();
            move |message: &str| {
                let _ = sender.send(TransportEvent::Log(log_message_notification(
                    MessageType::INFO,
                    message.to_owned(),
                )));
            }
        };
        // Drop the `Send` auto-trait from the shared reference inside the
        // worker closure; the callback itself is moved across threads, but
        // `prepare_initialize_candidate` only needs a `Sync` view.
        self.state = ServerState::Initializing;
        *in_flight = Some(InFlightInitialize {
            request_id: request_id.clone(),
            cancellation: cancellation.clone(),
        });
        scope.spawn(move || {
            let stage_ref: &(dyn Fn(&str) + Sync) = &stage;
            let log_ref: &(dyn Fn(&str) + Sync) = &log;
            for message in startup_log {
                log_ref(&message);
            }
            log_ref(&initialize_message);
            let callbacks = InitializeCallbacks {
                stage: Some(stage_ref),
                log: Some(log_ref),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prepare_initialize_candidate(
                    candidate,
                    params,
                    scan_workspace,
                    auto_vanilla.as_ref(),
                    &cancellation,
                    &callbacks,
                )
            }))
            .unwrap_or_else(|_| {
                Err(RpcError::new(
                    INTERNAL_ERROR,
                    "initialize worker failed unexpectedly",
                ))
            });
            let _ = sender.send(TransportEvent::Initialize(Box::new(InitializeTaskResult {
                request_id,
                id,
                result,
            })));
        });
        Ok(true)
    }

    pub(super) fn spawn_pending_disk_changes<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightDiskChanges>,
    ) {
        if !matches!(
            self.state,
            ServerState::Initialized | ServerState::ShuttingDown
        ) || in_flight.is_some()
            || !self.has_pending_disk_changes()
            || self
                .pending_disk_changes_due
                .is_some_and(|due| due > Instant::now())
        {
            return;
        }
        let full_rescan = self.pending_disk_changes_rescan;
        let changes = std::mem::take(&mut self.pending_disk_changes)
            .into_iter()
            .map(|(path, kind)| DiskFileChange::new(path, kind))
            .collect::<Vec<_>>();
        self.pending_disk_changes_due = None;
        self.pending_disk_changes_rescan = false;
        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_changes = changes.clone();
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        let publish_workspace_diagnostics = self.workspace_wide_diagnostics;
        let ignored_diagnostic_codes = Arc::clone(&self.ignored_diagnostic_codes);
        let diagnostics_cache = std::sync::Arc::clone(&self.workspace_diagnostics_cache);
        let diagnostic_severity_overrides = Arc::clone(&self.diagnostic_severity_overrides);
        *in_flight = Some(InFlightDiskChanges {
            base_revision,
            cancellation,
        });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if full_rescan {
                    candidate
                        .refresh_source_roots_cancellable(&worker_cancellation)
                        .and_then(|_| {
                            let validation = publish_workspace_diagnostics
                                .then(|| {
                                    workspace_validation_result(
                                        &candidate,
                                        &worker_cancellation,
                                        &ignored_diagnostic_codes,
                                        &diagnostic_severity_overrides,
                                        true,
                                        &diagnostics_cache,
                                    )
                                })
                                .transpose()?;
                            Ok((candidate, validation))
                        })
                } else {
                    candidate
                        .apply_disk_file_changes_cancellable(&worker_changes, &worker_cancellation)
                        .and_then(|_| {
                            // Incremental batches only re-validate the files
                            // they touched; re-running the whole-workspace
                            // pass per watcher burst burned one core per burst.
                            let validation = publish_workspace_diagnostics
                                .then(|| {
                                    changed_files_validation_result(
                                        &candidate,
                                        &worker_changes,
                                        &worker_cancellation,
                                        &ignored_diagnostic_codes,
                                        &diagnostic_severity_overrides,
                                    )
                                })
                                .transpose()?;
                            Ok((candidate, validation))
                        })
                }
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace file-change worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::DiskChanges(DiskChangesResult {
                base_revision,
                changes: worker_changes,
                result,
            }));
        });
    }

    pub(super) fn requeue_disk_changes(&mut self, changes: Vec<DiskFileChange>) {
        for change in changes {
            self.pending_disk_changes
                .entry(change.path)
                .or_insert(change.kind);
        }
        if self.has_pending_disk_changes() {
            self.pending_disk_changes_due = Some(Instant::now());
        }
    }

    pub(super) fn schedule_parse(&mut self, uri: &str) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_parses.insert(id, PendingParse { version });
        }
    }

    pub(super) fn cancel_stale_parses(&self, in_flight: &BTreeMap<DocumentId, InFlightParse>) {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot
                .document(id)
                .and_then(|document| document.version());
            let superseded = self
                .pending_parses
                .get(id)
                .is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                task.cancelled.store(true, Ordering::Release);
            }
        }
    }

    pub(super) fn spawn_pending_parses<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightParse>,
    ) {
        let ready = self
            .pending_parses
            .keys()
            .filter(|id| !in_flight.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_parses.remove(&id) else {
                continue;
            };
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            in_flight.insert(
                id.clone(),
                InFlightParse {
                    version: pending.version,
                    cancelled: Arc::clone(&cancelled),
                },
            );
            scope.spawn(move || {
                let prepared = if cancelled.load(Ordering::Acquire) {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        snapshot.prepare_document(&id)
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancelled.load(Ordering::Acquire))
                };
                let _ = sender.send(TransportEvent::Parse(ParseResult {
                    id,
                    version: pending.version,
                    prepared,
                }));
            });
        }
    }

    pub(super) fn schedule_diagnostics(&mut self, uri: &str, delay: Duration) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_diagnostics.insert(
                id,
                PendingDiagnostics {
                    uri: uri.to_owned(),
                    version,
                    due: Instant::now() + delay,
                },
            );
        }
    }

    pub(super) fn schedule_diagnostics_for_document(
        &mut self,
        id: DocumentId,
        version: i64,
        delay: Duration,
    ) {
        self.pending_diagnostics.insert(
            id.clone(),
            PendingDiagnostics {
                uri: id.as_str().to_owned(),
                version,
                due: Instant::now() + delay,
            },
        );
    }

    pub(super) fn cancel_stale_diagnostics<W: Write>(
        &self,
        in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>,
        output: &mut W,
    ) -> Result<(), LspError> {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot
                .document(id)
                .and_then(|document| document.version());
            let superseded = self
                .pending_diagnostics
                .get(id)
                .is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                // The stale check runs every loop iteration; trace the transition
                // only once per task or a cancelled round would spam one line per
                // iteration until its worker thread reports back.
                if !task.cancellation.is_cancelled() {
                    task.cancellation.cancel();
                    self.trace_decision(
                        output,
                        format!(
                            "document diagnostics superseded {} v{}; cancelled in flight",
                            uri_tail(id.as_str()),
                            task.version
                        ),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn next_diagnostic_wait(
        &self,
        in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>,
    ) -> Option<Duration> {
        let now = Instant::now();
        self.pending_diagnostics
            .iter()
            .filter(|(id, _)| !in_flight.contains_key(*id))
            .map(|(_, pending)| pending.due.saturating_duration_since(now))
            .min()
    }

    pub(super) fn spawn_due_diagnostics<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightDiagnostics>,
        force: bool,
        output: &mut W,
    ) -> Result<(), LspError> {
        let now = Instant::now();
        let ready = self
            .pending_diagnostics
            .iter()
            .filter(|(id, pending)| !in_flight.contains_key(*id) && (force || pending.due <= now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_diagnostics.remove(&id) else {
                continue;
            };
            self.trace_decision(
                output,
                format!(
                    "document diagnostics started {} v{}",
                    uri_tail(&pending.uri),
                    pending.version
                ),
            )?;
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancellation = CancellationToken::new();
            let ignored_diagnostic_codes = Arc::clone(&self.ignored_diagnostic_codes);
            let diagnostic_severity_overrides = Arc::clone(&self.diagnostic_severity_overrides);
            in_flight.insert(
                id.clone(),
                InFlightDiagnostics {
                    version: pending.version,
                    cancellation: cancellation.clone(),
                },
            );
            scope.spawn(move || {
                let values = if cancellation.is_cancelled() {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        diagnostic_values_with_ignored_and_overrides(
                            &snapshot,
                            &id,
                            &cancellation,
                            &ignored_diagnostic_codes,
                            &diagnostic_severity_overrides,
                        )
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancellation.is_cancelled())
                };
                let _ = sender.send(TransportEvent::Diagnostics(DiagnosticsResult {
                    id,
                    uri: pending.uri,
                    version: pending.version,
                    values,
                }));
            });
        }
        Ok(())
    }

    /// Drops CST/HIR frontends of source files once background validation
    /// has consumed them, and reports that validation finished.
    ///
    /// Open overlays live in the document map with their own trees, so every
    /// file state can be evicted; later queries reparse the retained source
    /// on demand. Shards and position ranges stay resident, keeping
    /// definition/reference/navigation answers intact while dropping the
    /// dominant CST/HIR memory share. The completion notice always fires:
    /// scan-time eviction means most runs release nothing here, but the
    /// workspace validation endpoint is still worth telling the user about.
    pub(super) fn evict_source_frontends_after_validation<W: Write>(
        &mut self,
        output: &mut W,
    ) -> Result<(), LspError> {
        let snapshot = self.host.snapshot();
        let open_overlay_files = snapshot
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
            .filter_map(|document| document.path())
            .filter_map(|path| snapshot.source_file_id_for_path(path))
            .collect::<HashSet<_>>();
        drop(snapshot);
        let evicted = self
            .host
            .evict_source_frontends(&|id| open_overlay_files.contains(&id));
        let detail = if evicted > 0 {
            format!("released syntax trees for {evicted} file(s); they reparse on demand")
        } else {
            "syntax trees stay released between queries".to_owned()
        };
        write_message(
            output,
            &log_message_notification(
                MessageType::INFO,
                format!("Workspace validation complete; {detail}"),
            ),
        )?;
        Ok(())
    }

    /// Starts the initial background workspace scan requested by the
    /// initialize handshake. The worker refreshes a host clone and reports
    /// back; the event loop commits it only while the live revision is
    /// unchanged, so concurrent document edits never lose their overlays.
    pub(super) fn spawn_pending_scan<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightScan>,
        output: &mut W,
    ) -> Result<(), LspError> {
        // ShuttingDown is accepted: the shutdown drain waits on `scan_pending`,
        // so a rescheduled scan that raced an edit must be able to respawn or
        // the drain would hold `exit` forever while dropping the scanned
        // workspace data. The retry counter still bounds the races.
        if !matches!(
            self.state,
            ServerState::Initialized | ServerState::ShuttingDown
        ) || in_flight.is_some()
            || !self.scan_pending
        {
            return Ok(());
        }
        self.scan_pending = false;
        self.scan_retries = self.scan_retries.saturating_add(1);
        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        let scan_gate = self.scan_gate.clone();
        let progress_token = format!("pdc-scan-{}", progress_nonce());
        let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
            if self.client_work_done_progress {
                write_message(output, &work_done_progress_create(&progress_token))?;
                write_message(
                    output,
                    &work_done_progress_begin(&progress_token, "Scanning workspace…"),
                )?;
                Some(Box::new(progress_sender(
                    sender.clone(),
                    progress_token.clone(),
                    "Scanning workspace",
                    "Indexing workspace files",
                )))
            } else {
                write_message(
                    output,
                    &show_info_notification("Scanning workspace in the background…".to_owned()),
                )?;
                None
            };
        let slot_token = if self.client_work_done_progress {
            Some(progress_token)
        } else {
            None
        };
        *in_flight = Some(InFlightScan {
            base_revision,
            cancellation,
            progress_token: slot_token,
        });
        let started = std::time::Instant::now();
        scope.spawn(move || {
            let progress_ref = progress
                .as_deref()
                .map(|f| f as &(dyn Fn(usize, usize) + Sync));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                candidate
                    .refresh_source_roots_cancellable_with_progress(
                        &worker_cancellation,
                        progress_ref,
                    )
                    .map(|report| (candidate, report))
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace scan worker failed unexpectedly",
                )))
            });
            // Test seam (never configured in production): park the finished
            // scan before its completion is reported so shutdown-race tests
            // can pin the interleaving deterministically.
            if let Some(gate) = scan_gate {
                gate.hold();
            }
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let _ = sender.send(TransportEvent::Log(log_message_notification(
                MessageType::INFO,
                format!("Background scan finished in {elapsed_ms:.1} ms"),
            )));
            let _ = sender.send(TransportEvent::ScanSetup(ScanSetupResult {
                base_revision,
                result,
            }));
        });
        Ok(())
    }

    /// Spawns the Vanilla/dependency cache workers configured by the
    /// initialize handshake, immediately after the initialize response so the
    /// cache loads overlap the initial background scan. The workers only read
    /// their inputs and build in-memory caches; the resulting install events
    /// are deferred by the event loop until the scan commits, so in-place
    /// cache installs never race the scan's host swap.
    ///
    /// Returns the in-flight slots and progress tokens for the event loop's
    /// completion handling; `is_load` marks the bounded cache-load flavor
    /// during which snapshot requests wait for complete answers.
    pub(super) fn spawn_background_cache_workers<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        index_cache: Option<PathBuf>,
        dependency_caches: Vec<DependencyIndexCache>,
        auto_vanilla: Option<AutoVanillaConfiguration>,
        output: &mut W,
    ) -> Result<CacheWorkersSpawned, LspError> {
        let mut spawned = CacheWorkersSpawned {
            index: None,
            index_progress_token: None,
            dependency: None,
            dependency_progress_token: None,
        };
        let mut in_flight_index_slot = None;
        let mut index_progress_token = None;
        let mut in_flight_dependency_slot = None;
        let mut dependency_progress_token = None;
        if let Some(path) = index_cache {
            write_message(
                output,
                &log_message_notification(
                    MessageType::INFO,
                    format!("Vanilla index: loading cache from {}", path.display()),
                ),
            )?;
            let cancellation = IndexSetupCancellation::new();
            let sender = event_sender.clone();
            let worker_cancellation = cancellation.clone();
            let rules = self.host.snapshot().rules().clone();
            let profile = self.host.snapshot().game_profile().clone();
            let scan_limits = self.host.snapshot().scan_limits();
            let preferred_localisation_languages = self
                .host
                .snapshot()
                .preferred_localisation_languages()
                .to_vec();
            let current_rule_hash = rules.rule_hash().to_hex();
            let progress_token = format!("pdc-vanilla-{}", progress_nonce());
            let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                if self.client_work_done_progress {
                    write_message(output, &work_done_progress_create(&progress_token))?;
                    write_message(
                        output,
                        &work_done_progress_begin(&progress_token, "Loading Vanilla index…"),
                    )?;
                    Some(Box::new(progress_sender(
                        sender.clone(),
                        progress_token.clone(),
                        "Loading Vanilla index",
                        "Loading Vanilla index",
                    )))
                } else {
                    write_message(
                        output,
                        &show_info_notification(
                            "Vanilla index is being loaded in the background…".to_owned(),
                        ),
                    )?;
                    None
                };
            index_progress_token = if self.client_work_done_progress {
                Some(progress_token.clone())
            } else {
                None
            };
            in_flight_index_slot = Some(InFlightIndexSlot {
                cancellation,
                is_load: true,
            });
            // The user-level auto configuration survives `apply_user_vanilla_configuration`,
            // so an unavailable configured cache can still fall back to discovery.
            let auto_vanilla = self.auto_vanilla.clone();
            let log = {
                let sender = event_sender.clone();
                move |message: &str| {
                    let _ = sender.send(TransportEvent::Log(log_message_notification(
                        MessageType::INFO,
                        message.to_owned(),
                    )));
                }
            };
            scope.spawn(move || {
                let log_ref: &(dyn Fn(&str) + Sync) = &log;
                let result = run_index_cache_load(IndexCacheLoadRequest {
                    path: &path,
                    rules,
                    profile,
                    current_rule_hash,
                    auto_vanilla: auto_vanilla.as_ref(),
                    log: Some(log_ref),
                    progress: progress
                        .as_deref()
                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                    scan_limits,
                    preferred_localisation_languages: &preferred_localisation_languages,
                    cancellation: &worker_cancellation,
                });
                let _ = sender.send(TransportEvent::VanillaSetup(IndexSetupResult { result }));
            });
        } else if let Some(configuration) = auto_vanilla {
            write_message(
                output,
                &log_message_notification(
                    MessageType::INFO,
                    "Vanilla index: discovering installation and building cache…".to_owned(),
                ),
            )?;
            let cancellation = IndexSetupCancellation::new();
            let sender = event_sender.clone();
            let rules = self.host.snapshot().rules().clone();
            let profile = self.host.snapshot().game_profile().clone();
            let scan_limits = self.host.snapshot().scan_limits();
            let worker_cancellation = cancellation.clone();
            let progress_token = format!("pdc-vanilla-{}", progress_nonce());
            let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                if self.client_work_done_progress {
                    write_message(output, &work_done_progress_create(&progress_token))?;
                    write_message(
                        output,
                        &work_done_progress_begin(&progress_token, "Building Vanilla index…"),
                    )?;
                    Some(Box::new(progress_sender(
                        sender.clone(),
                        progress_token.clone(),
                        "Discovering Vanilla files",
                        "Indexing Vanilla files",
                    )))
                } else {
                    write_message(
                        output,
                        &show_info_notification(
                            "Vanilla index is being built in the background…".to_owned(),
                        ),
                    )?;
                    None
                };
            index_progress_token = if self.client_work_done_progress {
                Some(progress_token.clone())
            } else {
                None
            };
            in_flight_index_slot = Some(InFlightIndexSlot {
                cancellation,
                is_load: false,
            });
            let log = {
                let sender = event_sender.clone();
                move |message: &str| {
                    let _ = sender.send(TransportEvent::Log(log_message_notification(
                        MessageType::INFO,
                        message.to_owned(),
                    )));
                }
            };
            scope.spawn(move || {
                let log_ref: &(dyn Fn(&str) + Sync) = &log;
                let result = crate::vanilla::run_auto_vanilla_setup_with_options_and_limits(
                    &configuration,
                    rules,
                    profile,
                    Some(log_ref),
                    progress
                        .as_deref()
                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                    &worker_cancellation,
                    &game::DiscoveryOptions::default(),
                    scan_limits,
                );
                let _ = sender.send(TransportEvent::VanillaSetup(IndexSetupResult { result }));
            });
        } else {
            write_message(
                                output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "Vanilla index: no cache or automatic discovery worker scheduled; continuing without Vanilla symbols"
                                        .to_owned(),
                                ),
                            )?;
        }
        if !dependency_caches.is_empty() {
            write_message(
                output,
                &log_message_notification(
                    MessageType::INFO,
                    format!(
                        "Dependency indexes: loading {} cache(s)…",
                        dependency_caches.len()
                    ),
                ),
            )?;
            let cancellation = WorkspaceScanToken::new();
            let sender = event_sender.clone();
            let worker_cancellation = cancellation.clone();
            let rules = self.host.snapshot().rules().clone();
            let profile = self.host.snapshot().game_profile().clone();
            let scan_limits = self.host.snapshot().scan_limits();
            let preferred_localisation_languages = self
                .host
                .snapshot()
                .preferred_localisation_languages()
                .to_vec();
            let current_rule_hash = rules.rule_hash().to_hex();
            let progress_token = format!("pdc-dependency-{}", progress_nonce());
            let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                if self.client_work_done_progress {
                    write_message(output, &work_done_progress_create(&progress_token))?;
                    write_message(
                        output,
                        &work_done_progress_begin(&progress_token, "Loading dependency index…"),
                    )?;
                    Some(Box::new(progress_sender(
                        sender.clone(),
                        progress_token.clone(),
                        "Loading dependency index",
                        "Indexing dependency files",
                    )))
                } else {
                    write_message(
                        output,
                        &show_info_notification(
                            "Dependency indexes are being loaded in the background…".to_owned(),
                        ),
                    )?;
                    None
                };
            dependency_progress_token = if self.client_work_done_progress {
                Some(progress_token.clone())
            } else {
                None
            };
            in_flight_dependency_slot = Some(InFlightDependencySlot { cancellation });
            let log = {
                let sender = event_sender.clone();
                move |message: &str| {
                    let _ = sender.send(TransportEvent::Log(log_message_notification(
                        MessageType::INFO,
                        message.to_owned(),
                    )));
                }
            };
            scope.spawn(move || {
                let results = crate::dependency::run_dependency_cache_loads(
                    dependency_caches,
                    rules,
                    profile,
                    current_rule_hash,
                    scan_limits,
                    &preferred_localisation_languages,
                    Some(&log),
                    progress
                        .as_deref()
                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                    &worker_cancellation,
                );
                let _ = sender.send(TransportEvent::DependencySetup(DependencySetupResult {
                    results,
                }));
            });
        } else {
            write_message(
                output,
                &log_message_notification(
                    MessageType::INFO,
                    "Dependency indexes: none configured".to_owned(),
                ),
            )?;
        }
        spawned.index = in_flight_index_slot;
        spawned.index_progress_token = index_progress_token;
        spawned.dependency = in_flight_dependency_slot;
        spawned.dependency_progress_token = dependency_progress_token;
        Ok(spawned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_reindex_wait_is_disabled_until_armed() {
        let mut server = LspServer::try_new(InitializeOptions).expect("identity server");
        server.state = ServerState::Initialized;
        assert!(server.background_reindex_wait(true, None, false).is_none());

        server.background_reindex_interval_minutes = 1;
        server.background_reindex_due = Some(Instant::now());
        server.background_reindex_idle_seconds = 0;
        assert_eq!(
            server.background_reindex_wait(true, None, false),
            Some(Duration::ZERO)
        );
        assert_eq!(
            server.background_reindex_wait(true, None, true),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn arm_background_reindex_respects_zero_interval() {
        let mut server = LspServer::try_new(InitializeOptions).expect("identity server");
        server.arm_background_reindex();
        assert!(server.background_reindex_due.is_none());

        server.background_reindex_interval_minutes = 1;
        server.arm_background_reindex();
        assert!(server.background_reindex_due.is_some());
    }

    #[test]
    fn watched_changes_reset_a_bounded_trailing_window() {
        let mut server = LspServer::try_new(InitializeOptions).expect("identity server");
        server.queue_watched_disk_change(
            AbsPath::normalize(&PathBuf::from("events/one.txt")),
            DiskFileChangeKind::Changed,
        );
        let first_due = server.pending_disk_changes_due.expect("debounce deadline");
        assert!(server.has_pending_disk_changes());
        assert!(
            server
                .pending_disk_change_wait(None)
                .is_some_and(|wait| { wait <= WATCHED_FILE_DEBOUNCE && wait > Duration::ZERO })
        );

        server.queue_watched_disk_change(
            AbsPath::normalize(&PathBuf::from("events/two.txt")),
            DiskFileChangeKind::Changed,
        );
        let second_due = server.pending_disk_changes_due.expect("reset deadline");
        assert!(second_due >= first_due);
        assert_eq!(server.pending_disk_changes.len(), 2);
        assert!(!server.pending_disk_changes_rescan);
    }

    #[test]
    fn watched_change_bulk_cap_switches_to_one_full_rescan() {
        let mut server = LspServer::try_new(InitializeOptions).expect("identity server");
        for index in 0..=WATCHED_BULK_CAP {
            server.queue_watched_disk_change(
                AbsPath::normalize(&PathBuf::from(format!("events/{index}.txt"))),
                DiskFileChangeKind::Changed,
            );
        }
        assert!(server.pending_disk_changes_rescan);
        assert!(server.pending_disk_changes.is_empty());
        assert!(server.has_pending_disk_changes());
        assert!(server.pending_disk_change_wait(None).is_some());
    }
}
