//! Definition-side projection of shared Template traversal coverage.
//! A repeated binding/scope state is unfinished analysis, not a game-runtime assertion.
use crate::semantic::dynamic_definition_type;
#[cfg(test)]
use crate::semantic::probe_query_cache;
use crate::support::ParsedInput;
use crate::{CancellationToken, Cancelled, Diagnostic, DiagnosticCertainty, DiagnosticCode};
use engine::AnalysisSnapshot;
#[cfg(test)]
use engine::DocumentSource;
use hir::analysis::AnalysisLimit;
#[cfg(test)]
use std::collections::BTreeSet;
use std::{collections::BTreeMap, sync::Arc};

#[cfg(test)]
const CACHE_KEY: &str = "template:definition-recursion-coverage";
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicCycleReport {
    entries: BTreeMap<(String, String), String>,
}
#[cfg(test)]
impl DynamicCycleReport {
    pub(crate) fn message(&self, kind: &str, name: &str) -> Option<&str> {
        self.entries
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .map(String::as_str)
    }
}
pub(crate) fn dynamic_cycle_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    let Some(hir) = input.hir.as_deref() else {
        return Ok(Vec::new());
    };
    if !hir
        .definitions()
        .iter()
        .any(|definition| dynamic_definition_type(snapshot, &definition.kind))
    {
        return Ok(Vec::new());
    }
    let mut diagnostics = Vec::new();
    for definition in hir
        .definitions()
        .iter()
        .filter(|definition| dynamic_definition_type(snapshot, &definition.kind))
    {
        if let Some(message) =
            cycle_message(snapshot, &definition.kind, &definition.name, cancellation)?
        {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::DynamicDefinitionCycle,
                    crate::Severity::Information,
                    definition.selection_range,
                    message,
                )
                .with_certainty(DiagnosticCertainty::Unresolved),
            );
        }
    }
    Ok(diagnostics)
}

fn cycle_message(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<String>, Cancelled> {
    cancellation.checkpoint()?;
    let key = format!(
        "template:recursion-coverage:{}:{}",
        kind.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    if let Some(message) = snapshot
        .query_cache()
        .get::<Option<String>>(snapshot.revision(), &key)
    {
        return Ok((*message).clone());
    }
    let analysed = crate::ir_template::missing_parameters(
        snapshot,
        kind,
        name,
        &BTreeMap::new(),
        hir::ScopeState {
            root: hir::ScopeValue::Unknown,
            current: vec![hir::ScopeValue::Unknown],
            previous: Vec::new(),
            from: Vec::new(),
        },
        cancellation,
    )?;
    let message = analysed.coverage.limits.contains(&AnalysisLimit::RecursiveState).then(||
        format!("Template `{name}` analysis repeats a binding/scope state with no arguments; expansion remains unknown"));
    snapshot.query_cache().insert(
        snapshot.revision(),
        engine::CacheDomain::Documents,
        key,
        Arc::new(message.clone()),
    );
    Ok(message)
}

#[cfg(test)]
pub(crate) fn dynamic_cycle_report(
    snapshot: &AnalysisSnapshot,
    cancellation: &CancellationToken,
) -> Result<Arc<DynamicCycleReport>, Cancelled> {
    let revision = snapshot.revision();
    if let Some(report) = probe_query_cache::<DynamicCycleReport>(snapshot, revision, &[CACHE_KEY])
    {
        return Ok(report);
    }
    if snapshot
        .query_cache()
        .is_superseded(engine::CacheDomain::Definitions, revision)
    {
        return Ok(Arc::new(DynamicCycleReport::default()));
    }
    let mut names = BTreeSet::new();
    for definition in snapshot.index().definitions_iter() {
        cancellation.checkpoint()?;
        if dynamic_definition_type(snapshot, &definition.kind) {
            names.insert((definition.kind.to_string(), definition.name.to_string()));
        }
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        for definition in document.hir().into_iter().flat_map(|hir| hir.definitions()) {
            if dynamic_definition_type(snapshot, &definition.kind) {
                names.insert((definition.kind.to_string(), definition.name.to_string()));
            }
        }
    }
    let mut report = DynamicCycleReport::default();
    for (kind, name) in names {
        if let Some(message) = cycle_message(snapshot, &kind, &name, cancellation)? {
            report.entries.insert(
                (kind.to_ascii_lowercase(), name.to_ascii_lowercase()),
                message,
            );
        }
    }
    let report = Arc::new(report);
    snapshot.query_cache().insert(
        revision,
        engine::CacheDomain::Definitions,
        CACHE_KEY.to_owned(),
        report.clone(),
    );
    Ok(report)
}
